using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ClaimCodeParserTests
{
    [Fact]
    public void Extract_reads_code_from_ansi_colored_tracing_line()
    {
        const string log =
            "\u001b[2m2026-11-14T07:00:00Z\u001b[0m \u001b[32mINFO\u001b[0m " +
            "\u001b[3mclaim_code\u001b[0m\u001b[2m=\u001b[0mCP-4P4EQ-ABWX2\n";

        Assert.Equal("CP-4P4EQ-ABWX2", ClaimCodeParser.Extract(log));
    }

    [Fact]
    public void Extract_returns_the_latest_code()
    {
        const string log =
            "claim_code=CP-AAAAA-BBBBB\n" +
            "claim_code=CP-CCCCC-DDDDD\n";

        Assert.Equal("CP-CCCCC-DDDDD", ClaimCodeParser.Extract(log));
    }

    [Fact]
    public void Extract_returns_null_without_a_code()
    {
        Assert.Null(ClaimCodeParser.Extract("agent started, no claim code yet"));
        Assert.Null(ClaimCodeParser.Extract(string.Empty));
        Assert.Null(ClaimCodeParser.Extract(null));
    }

    [Fact]
    public void StripAnsi_removes_color_sequences()
    {
        Assert.Equal("claim_code=CP-4P4EQ-ABWX2", ClaimCodeParser.StripAnsi(
            "\u001b[3mclaim_code\u001b[0m\u001b[2m=\u001b[0mCP-4P4EQ-ABWX2"));
    }
}
