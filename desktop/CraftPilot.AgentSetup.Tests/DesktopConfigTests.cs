using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class DesktopConfigTests
{
    [Fact]
    public void BuildEnvFile_uses_forward_slashes_and_expected_keys()
    {
        var dataDirectory = Path.Combine(Path.GetTempPath(), "craftpilot setup", "servers");

        var env = DesktopConfig.BuildEnvFile(dataDirectory);

        Assert.Contains("CRAFTPILOT_MASTER_URL=https://craftpilot.se", env);
        Assert.Contains("CRAFTPILOT_SERVER_DATA_ROOT=/srv/craftpilot/servers", env);
        Assert.Contains(
            $"CRAFTPILOT_HOST_DATA_DIR={dataDirectory.Replace('\\', '/')}",
            env);
        Assert.DoesNotContain("\\", env);
        Assert.EndsWith(Environment.NewLine, env);
    }

    [Fact]
    public void BuildEnvFile_writes_the_sftp_flag()
    {
        var dataDirectory = Path.Combine(Path.GetTempPath(), "servers");

        Assert.Contains(
            "CRAFTPILOT_SFTP_ENABLED=true",
            DesktopConfig.BuildEnvFile(dataDirectory));
        Assert.Contains(
            "CRAFTPILOT_SFTP_ENABLED=false",
            DesktopConfig.BuildEnvFile(dataDirectory, false));
    }

    [Fact]
    public void BuildEnvFile_keeps_settings_the_panel_does_not_own()
    {
        var dataDirectory = Path.Combine(Path.GetTempPath(), "servers");
        var existing = string.Join(
            Environment.NewLine,
            "CRAFTPILOT_HOST_DATA_DIR=D:/old",
            "CRAFTPILOT_RS_AGENT_IMAGE_TAG=0.11",
            "CRAFTPILOT_SFTP_PORT=2223",
            "CRAFTPILOT_SFTP_ENABLED=false");

        var env = DesktopConfig.BuildEnvFile(dataDirectory, true, existing);

        // Manuella rader överlever, panelens egna skrivs om.
        Assert.Contains("CRAFTPILOT_RS_AGENT_IMAGE_TAG=0.11", env);
        Assert.Contains("CRAFTPILOT_SFTP_PORT=2223", env);
        Assert.DoesNotContain("D:/old", env);
        Assert.DoesNotContain("CRAFTPILOT_SFTP_ENABLED=false", env);
        Assert.Single(
            env.Split(Environment.NewLine),
            line => line.StartsWith("CRAFTPILOT_SFTP_ENABLED=", StringComparison.Ordinal));
    }

    [Fact]
    public void ReadSftpEnabled_defaults_to_on()
    {
        Assert.True(DesktopConfig.ReadSftpEnabled(null));
        Assert.True(DesktopConfig.ReadSftpEnabled("RUST_LOG=info"));
        Assert.True(DesktopConfig.ReadSftpEnabled("CRAFTPILOT_SFTP_ENABLED=true"));
        Assert.False(DesktopConfig.ReadSftpEnabled("CRAFTPILOT_SFTP_ENABLED=false"));
        Assert.False(DesktopConfig.ReadSftpEnabled("CRAFTPILOT_SFTP_ENABLED=0"));
    }
}
