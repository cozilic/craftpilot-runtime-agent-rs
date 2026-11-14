using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class DockerStateClassifierTests
{
    [Theory]
    [InlineData(0, "", false, DockerStateClassifier.Ready)]
    [InlineData(-1, "The system cannot find the file specified.", true, DockerStateClassifier.NotInstalled)]
    [InlineData(1, "docker: command not found", false, DockerStateClassifier.NotInstalled)]
    [InlineData(1, "'docker' is not recognized as an internal or external command", false, DockerStateClassifier.NotInstalled)]
    [InlineData(1, "error during connect: open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified.", false, DockerStateClassifier.NotRunning)]
    [InlineData(1, "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?", false, DockerStateClassifier.NotRunning)]
    [InlineData(1, "\u001b[31merror during connect\u001b[0m", false, DockerStateClassifier.NotRunning)]
    [InlineData(1, "permission denied while trying to connect to the Docker daemon socket", false, DockerStateClassifier.Error)]
    [InlineData(1, "Tidsgränsen överskreds.", false, DockerStateClassifier.Error)]
    public void Classify_returns_the_expected_state(int exitCode, string output, bool startFailed, string expected)
    {
        Assert.Equal(expected, DockerStateClassifier.Classify(exitCode, output, startFailed));
    }
}
