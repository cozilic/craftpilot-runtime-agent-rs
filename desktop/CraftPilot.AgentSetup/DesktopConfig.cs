namespace CraftPilot.AgentSetup;

public static class DesktopConfig
{
    public const string ServerDataRoot = "/srv/craftpilot/servers";
    public const string SftpEnabledKey = "CRAFTPILOT_SFTP_ENABLED";

    /// <summary>Raderna panelen äger; allt annat i .env.desktop bevaras.</summary>
    private static readonly HashSet<string> ManagedKeys = new(StringComparer.Ordinal)
    {
        "CRAFTPILOT_HOST_DATA_DIR",
        "CRAFTPILOT_SERVER_DATA_ROOT",
        "CRAFTPILOT_DOCKER_SERVER_DATA_ROOT",
        "CRAFTPILOT_MASTER_URL",
        "CRAFTPILOT_POLL_SECONDS",
        "RUST_LOG",
        SftpEnabledKey,
    };

    /// <summary>
    /// Bygger .env.desktop. Rader panelen inte äger — t.ex.
    /// CRAFTPILOT_RS_AGENT_IMAGE_TAG eller en manuellt satt CRAFTPILOT_SFTP_PORT
    /// — kopieras från <paramref name="existing"/>, så en installation eller
    /// uppdatering inte nollställer dem.
    /// </summary>
    public static string BuildEnvFile(string dataDirectory, bool sftpEnabled = true, string? existing = null)
    {
        var hostPath = Path.GetFullPath(dataDirectory).Replace('\\', '/');
        var lines = new List<string>
        {
            $"CRAFTPILOT_HOST_DATA_DIR={hostPath}",
            $"CRAFTPILOT_SERVER_DATA_ROOT={ServerDataRoot}",
            $"CRAFTPILOT_DOCKER_SERVER_DATA_ROOT={hostPath}",
            $"CRAFTPILOT_MASTER_URL={MasterUrl.Default}",
            "CRAFTPILOT_POLL_SECONDS=5",
            "RUST_LOG=info",
            $"{SftpEnabledKey}={(sftpEnabled ? "true" : "false")}",
        };

        foreach (var line in (existing ?? string.Empty).Split('\n'))
        {
            var trimmed = line.Trim();
            var separator = trimmed.IndexOf('=');
            if (trimmed.Length == 0 || separator <= 0)
            {
                continue;
            }

            var key = trimmed[..separator];
            if (!ManagedKeys.Contains(key) &&
                !lines.Any(entry => entry.StartsWith($"{key}=", StringComparison.Ordinal)))
            {
                lines.Add(trimmed);
            }
        }

        return string.Join(Environment.NewLine, lines) + Environment.NewLine;
    }

    /// <summary>SFTP-läget ur en befintlig .env.desktop. På är default.</summary>
    public static bool ReadSftpEnabled(string? existing)
    {
        foreach (var line in (existing ?? string.Empty).Split('\n'))
        {
            var separator = line.IndexOf('=');
            if (separator > 0 &&
                string.Equals(line[..separator].Trim(), SftpEnabledKey, StringComparison.Ordinal))
            {
                return SftpStatus.IsEnabled(line[(separator + 1)..]);
            }
        }

        return true;
    }
}
