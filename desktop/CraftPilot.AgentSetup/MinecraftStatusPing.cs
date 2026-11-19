using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Minimal Minecraft server-list-ping (status-protokollet) mot en publicerad
/// serverport. Panelen använder den bara för att visa spelarantal; allt annat
/// om servern kommer från Docker och agenten.
/// </summary>
public static class MinecraftStatusPing
{
    private const int ProtocolVersion = 47;
    private const int MaxPacketBytes = 2 * 1024 * 1024;

    public static async Task<(int Online, int Max)?> QueryAsync(
        string host,
        int port,
        TimeSpan timeout,
        CancellationToken ct = default)
    {
        using var timeoutSource = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeoutSource.CancelAfter(timeout);

        try
        {
            using var client = new TcpClient();
            await client.ConnectAsync(host, port, timeoutSource.Token);
            await using var stream = client.GetStream();

            await stream.WriteAsync(BuildHandshake(host, port), timeoutSource.Token);
            // Status request: längd 1, packet-id 0.
            await stream.WriteAsync(new byte[] { 0x01, 0x00 }, timeoutSource.Token);

            var length = await ReadVarIntAsync(stream, timeoutSource.Token);
            if (length <= 0 || length > MaxPacketBytes)
            {
                return null;
            }

            var payload = new byte[length];
            await stream.ReadExactlyAsync(payload, timeoutSource.Token);

            return ParseStatus(payload);
        }
        catch (Exception ex) when (
            ex is SocketException or IOException or OperationCanceledException
                or JsonException or InvalidDataException or ArgumentException)
        {
            // En server som inte svarar är inte ett fel i panelen: spelarantalet
            // visas som okänt i stället.
            return null;
        }
    }

    private static byte[] BuildHandshake(string host, int port)
    {
        using var body = new MemoryStream();
        WriteVarInt(body, 0x00);
        WriteVarInt(body, ProtocolVersion);
        WriteString(body, host);
        body.WriteByte((byte)(port >> 8));
        body.WriteByte((byte)(port & 0xFF));
        WriteVarInt(body, 1);

        using var packet = new MemoryStream();
        WriteVarInt(packet, (int)body.Length);
        body.Position = 0;
        body.CopyTo(packet);
        return packet.ToArray();
    }

    private static (int Online, int Max)? ParseStatus(byte[] payload)
    {
        var reader = new PacketReader(payload);
        if (reader.ReadVarInt() != 0x00)
        {
            return null;
        }

        var json = reader.ReadString();
        using var document = JsonDocument.Parse(json);
        if (!document.RootElement.TryGetProperty("players", out var players))
        {
            return null;
        }

        var online = players.TryGetProperty("online", out var onlineValue) && onlineValue.TryGetInt32(out var value)
            ? value
            : 0;
        var max = players.TryGetProperty("max", out var maxValue) && maxValue.TryGetInt32(out var maxPlayers)
            ? maxPlayers
            : 0;

        return (online, max);
    }

    private static async Task<int> ReadVarIntAsync(NetworkStream stream, CancellationToken ct)
    {
        var result = 0;
        for (var shift = 0; shift < 35; shift += 7)
        {
            var buffer = new byte[1];
            await stream.ReadExactlyAsync(buffer, ct);
            result |= (buffer[0] & 0x7F) << shift;
            if ((buffer[0] & 0x80) == 0)
            {
                return result;
            }
        }

        throw new InvalidDataException("VarInt är för långt.");
    }

    private static void WriteVarInt(Stream stream, int value)
    {
        var remaining = (uint)value;
        do
        {
            var current = (byte)(remaining & 0x7F);
            remaining >>= 7;
            if (remaining != 0)
            {
                current |= 0x80;
            }

            stream.WriteByte(current);
        }
        while (remaining != 0);
    }

    private static void WriteString(Stream stream, string value)
    {
        var bytes = Encoding.UTF8.GetBytes(value);
        WriteVarInt(stream, bytes.Length);
        stream.Write(bytes);
    }

    /// <summary>Läsare för svarets fält (VarInt + längdprefixad UTF-8).</summary>
    private sealed class PacketReader(byte[] buffer)
    {
        private int _offset;

        public int ReadVarInt()
        {
            var result = 0;
            for (var shift = 0; shift < 35; shift += 7)
            {
                if (_offset >= buffer.Length)
                {
                    throw new InvalidDataException("Paketet tog slut.");
                }

                var current = buffer[_offset++];
                result |= (current & 0x7F) << shift;
                if ((current & 0x80) == 0)
                {
                    return result;
                }
            }

            throw new InvalidDataException("VarInt är för långt.");
        }

        public string ReadString()
        {
            var length = ReadVarInt();
            if (length < 0 || length > MaxPacketBytes || _offset + length > buffer.Length)
            {
                throw new InvalidDataException("Ogiltig stränglängd.");
            }

            var value = Encoding.UTF8.GetString(buffer, _offset, length);
            _offset += length;
            return value;
        }
    }
}
