using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ClaimCodeParserTests
{
    [Fact]
    public void ExtractPending_reads_code_from_ansi_colored_tracing_line()
    {
        const string log =
            "\u001b[2m2026-11-14T07:00:00Z\u001b[0m \u001b[32mINFO\u001b[0m " +
            "\u001b[3mclaim_code\u001b[0m\u001b[2m=\u001b[0mCP-4P4EQ-ABWX2\n";

        Assert.Equal("CP-4P4EQ-ABWX2", ClaimCodeParser.ExtractPending(log));
    }

    [Fact]
    public void ExtractPending_returns_the_latest_code()
    {
        const string log =
            "claim_code=CP-AAAAA-BBBBB\n" +
            "claim_code=CP-CCCCC-DDDDD\n";

        Assert.Equal("CP-CCCCC-DDDDD", ClaimCodeParser.ExtractPending(log));
    }

    [Fact]
    public void ExtractPending_returns_null_without_a_code()
    {
        Assert.Null(ClaimCodeParser.ExtractPending("agent started, no claim code yet"));
        Assert.Null(ClaimCodeParser.ExtractPending(string.Empty));
        Assert.Null(ClaimCodeParser.ExtractPending(null));
    }

    [Fact]
    public void ExtractPending_returns_the_new_code_after_a_removed_node_re_enrolled()
    {
        const string log =
            "claim_code=CP-AAAAA-BBBBB\n" +
            "enrollment completed node_id=3d2529cc-26bb-4601-bf21-364e4ce6ee0a\n" +
            "the master rejected the credential — filing a replacement enrollment request\n" +
            "claim_code=CP-CCCCC-DDDDD\n";

        Assert.Equal("CP-CCCCC-DDDDD", ClaimCodeParser.ExtractPending(log));
    }

    [Fact]
    public void ExtractPending_ignores_the_code_of_a_completed_enrollment()
    {
        const string log =
            "claim_code=CP-AAAAA-BBBBB\n" +
            "enrollment completed node_id=3d2529cc-26bb-4601-bf21-364e4ce6ee0a\n";

        Assert.Null(ClaimCodeParser.ExtractPending(log));
    }

    [Fact]
    public void StripAnsi_removes_color_sequences()
    {
        Assert.Equal("claim_code=CP-4P4EQ-ABWX2", ClaimCodeParser.StripAnsi(
            "\u001b[3mclaim_code\u001b[0m\u001b[2m=\u001b[0mCP-4P4EQ-ABWX2"));
    }
}
