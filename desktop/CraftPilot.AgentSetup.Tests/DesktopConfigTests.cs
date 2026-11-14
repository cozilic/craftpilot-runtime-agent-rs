using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class DesktopConfigTests
{
    [Fact]
    public void BuildEnvFile_uses_forward_slashes_and_expected_keys()
    {
        var dataDirectory = Path.Combine(Path.GetTempPath(), "craftpilot setup", "servers");

        var env = DesktopConfig.BuildEnvFile("https://craftpilot.se", dataDirectory);

        Assert.Contains("CRAFTPILOT_MASTER_URL=https://craftpilot.se", env);
        Assert.Contains("CRAFTPILOT_SERVER_DATA_ROOT=/srv/craftpilot/servers", env);
        Assert.Contains(
            $"CRAFTPILOT_HOST_DATA_DIR={dataDirectory.Replace('\\', '/')}",
            env);
        Assert.DoesNotContain("\\", env);
        Assert.EndsWith(Environment.NewLine, env);
    }
}
