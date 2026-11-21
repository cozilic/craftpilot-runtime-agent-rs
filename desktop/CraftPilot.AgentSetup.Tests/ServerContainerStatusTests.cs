using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ServerContainerStatusTests
{
    [Fact]
    public void Parse_reads_the_exit_code_from_docker_status()
    {
        var servers = ServerContainers.Parse(
            "craftpilot-096d46aa|096d46aa-1111-2222-3333-444455556666|exited|img||Exited (1) 2 minutes ago\n" +
            "craftpilot-134eb9b3|134eb9b3-314F-4454-B53E-24097E0F1A07|running|img||Up 3 hours\n");

        Assert.Equal(1, Assert.Single(servers, server => server.Name == "craftpilot-096d46aa").ExitCode);
        Assert.Null(Assert.Single(servers, server => server.Name == "craftpilot-134eb9b3").ExitCode);
    }

    [Fact]
    public void ExitCodeFrom_ignores_statuses_without_a_code()
    {
        Assert.Equal(137, ServerContainers.ExitCodeFrom("Exited (137) 5 seconds ago"));
        Assert.Null(ServerContainers.ExitCodeFrom("Up 2 hours"));
        Assert.Null(ServerContainers.ExitCodeFrom("Created"));
        Assert.Null(ServerContainers.ExitCodeFrom(null));
    }

    [Fact]
    public void ExpectedName_matches_the_agents_container_name()
    {
        Assert.Equal(
            "craftpilot-096d46aa",
            ServerContainers.ExpectedName("096d46aa-1111-2222-3333-444455556666"));
        Assert.Equal(
            "craftpilot-134eb9b3",
            ServerContainers.ExpectedName("134EB9B3-314F-4454-B53E-24097E0F1A07"));
        Assert.Equal(string.Empty, ServerContainers.ExpectedName(null));
        Assert.Equal(string.Empty, ServerContainers.ExpectedName("inte-ett-id"));
    }
}
