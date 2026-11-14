using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class StartupRegistrationTests
{
    [Fact]
    public void BuildCommand_quotes_the_executable_and_adds_the_startup_flag()
    {
        var executable = Path.Combine(
            Path.GetTempPath(),
            "CraftPilot Panel",
            "CraftPilot.AgentSetup.exe");

        var command = StartupRegistration.BuildCommand(executable);

        Assert.Equal($"\"{Path.GetFullPath(executable)}\" --startup", command);
    }

    [Theory]
    [InlineData(new[] { "--startup" }, true)]
    [InlineData(new[] { "--tray" }, true)]
    [InlineData(new[] { "--startup", "--other" }, true)]
    [InlineData(new[] { "--other" }, false)]
    [InlineData(new string[0], false)]
    public void IsStartupInvocation_detects_tray_launches(string[] args, bool expected)
    {
        Assert.Equal(expected, StartupRegistration.IsStartupInvocation(args));
    }
}
