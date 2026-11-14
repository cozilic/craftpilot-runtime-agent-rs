using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class TrayStatusTests
{
    [Fact]
    public void Busy_has_priority()
    {
        var status = TrayStatusClassifier.From(Status(busy: true));

        Assert.Equal(TrayState.Busy, status.State);
        Assert.Equal("Arbetar…", status.Label);
    }

    [Fact]
    public void Docker_failure_is_bad()
    {
        var status = TrayStatusClassifier.From(Status(docker: false));

        Assert.Equal(TrayState.Bad, status.State);
        Assert.Equal("Docker är inte redo", status.Label);
    }

    [Fact]
    public void Master_failure_is_bad()
    {
        var status = TrayStatusClassifier.From(Status(master: false));

        Assert.Equal(TrayState.Bad, status.State);
        Assert.Equal("Mastern svarar inte", status.Label);
    }

    [Fact]
    public void Running_agent_is_ok()
    {
        var status = TrayStatusClassifier.From(Status(agentRunning: true, installed: true));

        Assert.Equal(TrayState.Ok, status.State);
        Assert.Equal("Agenten är igång", status.Label);
    }

    [Fact]
    public void Stopped_installed_agent_is_a_warning()
    {
        var status = TrayStatusClassifier.From(Status(installed: true));

        Assert.Equal(TrayState.Warn, status.State);
        Assert.Equal("Agenten är stoppad", status.Label);
    }

    [Fact]
    public void Missing_agent_is_a_warning()
    {
        var status = TrayStatusClassifier.From(Status());

        Assert.Equal(TrayState.Warn, status.State);
        Assert.Equal("Ingen agent installerad", status.Label);
    }

    private static StatusResponse Status(
        bool docker = true,
        bool master = true,
        bool installed = false,
        bool agentRunning = false,
        bool busy = false) =>
        new()
        {
            DockerOk = docker,
            MasterOk = master,
            Installed = installed,
            AgentRunning = agentRunning,
            Busy = busy,
        };
}
