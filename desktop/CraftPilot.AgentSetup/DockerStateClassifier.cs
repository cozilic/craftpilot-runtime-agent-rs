namespace CraftPilot.AgentSetup;

public static class DockerStateClassifier
{
    public const string Ready = "ready";
    public const string NotInstalled = "not-installed";
    public const string NotRunning = "not-running";
    public const string Error = "error";

    public static string Classify(int exitCode, string output, bool startFailed)
    {
        if (exitCode == 0)
        {
            return Ready;
        }

        var text = ClaimCodeParser.StripAnsi(output);

        if (startFailed ||
            text.Contains("command not found", StringComparison.OrdinalIgnoreCase) ||
            text.Contains("is not recognized as an internal or external command", StringComparison.OrdinalIgnoreCase))
        {
            return NotInstalled;
        }

        if (text.Contains("cannot connect to the docker daemon", StringComparison.OrdinalIgnoreCase) ||
            text.Contains("is the docker daemon running", StringComparison.OrdinalIgnoreCase) ||
            text.Contains("error during connect", StringComparison.OrdinalIgnoreCase) ||
            text.Contains("open //./pipe/docker", StringComparison.OrdinalIgnoreCase))
        {
            return NotRunning;
        }

        if (text.Contains("cannot find the file specified", StringComparison.OrdinalIgnoreCase))
        {
            return NotInstalled;
        }

        return Error;
    }
}
