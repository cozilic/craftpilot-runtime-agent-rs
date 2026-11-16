using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ServerContainersTests
{
    private const string Sample =
        "craftpilot-096d46aa|096d46aa-1111-2222-3333-444455556666|running|itzg/minecraft-server:java21\r\n" +
        "craftpilot-134eb9b3|134EB9B3-314F-4454-B53E-24097E0F1A07|exited|itzg/minecraft-server:java17\r\n";

    [Fact]
    public void Parse_keeps_every_server_and_puts_running_first()
    {
        var servers = ServerContainers.Parse(Sample);

        Assert.Equal(2, servers.Count);
        Assert.Equal("craftpilot-096d46aa", servers[0].Name);
        Assert.True(servers[0].Running);
        Assert.Equal("096d46aa", servers[0].ShortId);
        Assert.Equal("Körs", servers[0].StateLabel);
        Assert.Equal("craftpilot-134eb9b3", servers[1].Name);
        Assert.False(servers[1].Running);
        Assert.Equal("Stoppad", servers[1].StateLabel);
    }

    [Fact]
    public void Parse_skips_lines_without_a_server_id_or_the_wrong_shape()
    {
        var servers = ServerContainers.Parse(
            "craftpilot-without-label||running|img\r\n" +
            "craftpilot-096d46aa|096d46aa|running\r\n" +
            "\r\n");

        Assert.Empty(servers);
    }

    [Fact]
    public void Stopped_keeps_only_managed_containers_that_are_not_running()
    {
        var servers = ServerContainers.Parse(
            "craftpilot-4f2b2d3a|4f2b2d3a-1111-2222-3333-444455556666|running|img\r\n" +
            "craftpilot-f24fdcd7|f24fdcd7-1111-2222-3333-444455556666|exited|img\r\n" +
            "another-container|11111111-1111-1111-1111-111111111111|exited|img\r\n" +
            "craftpilot-134eb9b3|134eb9b3-314F-4454-B53E-24097E0F1A07|created|img\r\n");

        var stopped = ServerContainers.Stopped(servers);

        Assert.Equal(2, stopped.Count);
        Assert.All(stopped, server => Assert.False(server.Running));
        Assert.Equal(
            new[] { "craftpilot-134eb9b3", "craftpilot-f24fdcd7" },
            stopped.Select(server => server.Name).OrderBy(name => name, StringComparer.Ordinal));
    }

    [Fact]
    public void IsManagedName_requires_the_agents_container_name()
    {
        Assert.True(ServerContainers.IsManagedName("craftpilot-096d46aa"));
        Assert.False(ServerContainers.IsManagedName("craftpilot-096d46a"));
        Assert.False(ServerContainers.IsManagedName("craftpilot-096D46AA"));
        Assert.False(ServerContainers.IsManagedName("craftpilot-096d46agg"));
        Assert.False(ServerContainers.IsManagedName("postgres"));
        Assert.False(ServerContainers.IsManagedName(null));
    }

    [Fact]
    public void Parse_returns_empty_for_empty_output()
    {
        Assert.Empty(ServerContainers.Parse(null));
        Assert.Empty(ServerContainers.Parse(string.Empty));
        Assert.Empty(ServerContainers.Parse("   \r\n"));
    }
}
