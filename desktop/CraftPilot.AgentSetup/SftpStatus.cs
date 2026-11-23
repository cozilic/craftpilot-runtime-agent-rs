using System.Globalization;
using System.Net.Sockets;
using System.Text.Json;

namespace CraftPilot.AgentSetup;

/// <summary>
/// SFTPGo-tjänsten som agenten startar (<c>craftpilot-sftpgo</c>). Panelen
/// läser den publicerade värdporten ur Docker — agenten flyttar till nästa
/// lediga port när den föredragna är upptagen — och provar en TCP-anslutning,
/// så porten och tjänstens svar syns utan att man letar i agentloggen.
/// </summary>
public sealed record SftpStatus
{
    public const string ContainerName = "craftpilot-sftpgo";

    /// <summary>Porten ur .env.desktop (<c>CRAFTPILOT_SFTP_PORT</c>), null när den inte är satt.</summary>
    public int? PreferredPort { get; init; }

    /// <summary>Porten containern publicerar på värden (agentens val), null utan container.</summary>
    public int? BoundPort { get; init; }

    public bool Running { get; init; }

    /// <summary>TCP-anslutning till porten på 127.0.0.1 lyckades; null när den inte provades.</summary>
    public bool? Responding { get; init; }

    public string? Error { get; init; }

    /// <summary>Agenten fick inte den föredragna porten och tog nästa lediga.</summary>
    public bool MovedFromPreferred =>
        PreferredPort is not null && BoundPort is not null && PreferredPort != BoundPort;

    /// <summary><c>CRAFTPILOT_SFTP_ENABLED</c> är på om den inte satts till false.</summary>
    public static bool IsEnabled(string? value)
    {
        var text = (value ?? string.Empty).Trim();
        return !string.Equals(text, "false", StringComparison.OrdinalIgnoreCase) &&
            !string.Equals(text, "0", StringComparison.OrdinalIgnoreCase) &&
            !string.Equals(text, "no", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Porten ur en env-rad; tomma och ogiltiga värden blir null.</summary>
    public static int? ParsePort(string? value) =>
        int.TryParse((value ?? string.Empty).Trim(), NumberStyles.Integer, CultureInfo.InvariantCulture, out var port) &&
        port is > 0 and <= 65535
            ? port
            : null;

    /// <summary>
    /// Värdporten ur <c>docker inspect --format '{{json .HostConfig.PortBindings}}'</c>,
    /// till exempel <c>{"2022/tcp":[{"HostIp":"0.0.0.0","HostPort":"2223"}]}</c>.
    /// Första giltiga tcp-bindningen vinner; containerns egen port är alltid 2022.
    /// </summary>
    public static int? BoundPortFrom(string? portBindingsJson)
    {
        if (string.IsNullOrWhiteSpace(portBindingsJson))
        {
            return null;
        }

        try
        {
            using var document = JsonDocument.Parse(portBindingsJson);
            if (document.RootElement.ValueKind != JsonValueKind.Object)
            {
                return null;
            }

            foreach (var binding in document.RootElement.EnumerateObject())
            {
                if (!binding.Name.EndsWith("/tcp", StringComparison.OrdinalIgnoreCase) ||
                    binding.Value.ValueKind != JsonValueKind.Array)
                {
                    continue;
                }

                foreach (var entry in binding.Value.EnumerateArray())
                {
                    if (entry.ValueKind == JsonValueKind.Object &&
                        entry.TryGetProperty("HostPort", out var hostPort) &&
                        ParsePort(hostPort.GetString()) is { } port)
                    {
                        return port;
                    }
                }
            }
        }
        catch (JsonException)
        {
            // Ett oväntat svar från Docker betyder bara att porten är okänd.
        }

        return null;
    }

    /// <summary>Sant när något lyssnar på porten på den här datorn.</summary>
    public static async Task<bool> ProbeAsync(int port, TimeSpan timeout, CancellationToken ct = default)
    {
        using var timeoutSource = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeoutSource.CancelAfter(timeout);

        try
        {
            using var client = new TcpClient();
            await client.ConnectAsync("127.0.0.1", port, timeoutSource.Token);
            return true;
        }
        catch (Exception ex) when (ex is SocketException or OperationCanceledException or ArgumentException)
        {
            // En stängd port är ett svar i sig: panelen visar den som "Inget svar".
            return false;
        }
    }
}
