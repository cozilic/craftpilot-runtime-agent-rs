using System.Net;
using System.Net.Sockets;
using System.Text;
using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class MinecraftStatusPingTests
{
    [Fact]
    public async Task QueryAsync_reads_online_and_max_from_a_server_list_response()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;

        var server = Task.Run(async () =>
        {
            using var client = await listener.AcceptTcpClientAsync();
            await using var stream = client.GetStream();

            var handshakeLength = await ReadVarIntAsync(stream);
            var handshake = new byte[handshakeLength];
            await stream.ReadExactlyAsync(handshake);

            // Status request: längd 1, packet-id 0.
            var request = new byte[2];
            await stream.ReadExactlyAsync(request);

            var json = "{\"version\":{\"name\":\"1.21\",\"protocol\":767},\"players\":{\"online\":5,\"max\":20}}";
            await stream.WriteAsync(BuildStatusPacket(json));
        });

        try
        {
            var result = await MinecraftStatusPing.QueryAsync(
                "127.0.0.1",
                port,
                TimeSpan.FromSeconds(3));

            Assert.Equal((5, 20), result);
            await server.WaitAsync(TimeSpan.FromSeconds(3));
        }
        finally
        {
            listener.Stop();
        }
    }

    private static async Task<int> ReadVarIntAsync(NetworkStream stream)
    {
        var result = 0;
        for (var shift = 0; shift < 35; shift += 7)
        {
            var buffer = new byte[1];
            await stream.ReadExactlyAsync(buffer);
            result |= (buffer[0] & 0x7F) << shift;
            if ((buffer[0] & 0x80) == 0)
            {
                return result;
            }
        }

        throw new InvalidDataException("VarInt är för långt.");
    }

    private static byte[] BuildStatusPacket(string json)
    {
        var text = Encoding.UTF8.GetBytes(json);
        using var body = new MemoryStream();
        WriteVarInt(body, 0x00);
        WriteVarInt(body, text.Length);
        body.Write(text);

        using var packet = new MemoryStream();
        WriteVarInt(packet, (int)body.Length);
        body.Position = 0;
        body.CopyTo(packet);
        return packet.ToArray();
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
}
