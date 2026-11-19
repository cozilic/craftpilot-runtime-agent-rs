using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class NodeNameParserTests
{
    [Fact]
    public void Extract_reads_the_name_from_a_tracing_line()
    {
        const string log =
            "2026-11-19T09:12:00Z  INFO craftpilot_runtime_agent::agent: node name node_name=silksong\n";

        Assert.Equal("silksong", NodeNameParser.Extract(log));
    }

    [Fact]
    public void Extract_handles_ansi_colored_field_names_and_spaces()
    {
        const string log =
            "\u001b[2m2026-11-19T09:12:00Z\u001b[0m \u001b[32mINFO\u001b[0m " +
            "\u001b[3mnode_name\u001b[0m\u001b[2m=\u001b[0mSilk song\n";

        Assert.Equal("Silk song", NodeNameParser.Extract(log));
    }

    [Fact]
    public void Extract_returns_the_latest_name()
    {
        const string log = "node_name=old\nnode_name=new\n";

        Assert.Equal("new", NodeNameParser.Extract(log));
    }

    [Fact]
    public void Extract_returns_null_without_a_name()
    {
        Assert.Null(NodeNameParser.Extract("agent started"));
        Assert.Null(NodeNameParser.Extract("node_name=   \n"));
        Assert.Null(NodeNameParser.Extract(string.Empty));
        Assert.Null(NodeNameParser.Extract(null));
    }
}
