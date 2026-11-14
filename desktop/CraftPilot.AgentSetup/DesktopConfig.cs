namespace CraftPilot.AgentSetup;

public static class DesktopConfig
{
    public const string ServerDataRoot = "/srv/craftpilot/servers";

    public static string BuildEnvFile(string masterUrl, string dataDirectory)
    {
        var hostPath = Path.GetFullPath(dataDirectory).Replace('\\', '/');

        return string.Join(
            Environment.NewLine,
            $"CRAFTPILOT_HOST_DATA_DIR={hostPath}",
            $"CRAFTPILOT_SERVER_DATA_ROOT={ServerDataRoot}",
            $"CRAFTPILOT_DOCKER_SERVER_DATA_ROOT={hostPath}",
            $"CRAFTPILOT_MASTER_URL={masterUrl}",
            "CRAFTPILOT_POLL_SECONDS=5",
            "RUST_LOG=info",
            string.Empty);
    }
}
