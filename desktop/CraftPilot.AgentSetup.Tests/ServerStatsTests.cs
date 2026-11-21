using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ServerStatsTests
{
    [Fact]
    public void Parse_reads_cpu_and_memory_for_each_container()
    {
        var usage = ServerStats.Parse(
            "craftpilot-096d46aa|12.50%|345.6MiB / 1GiB\n" +
            "craftpilot-134eb9b3|0.00%|0B / 2GiB\n");

        var first = usage["craftpilot-096d46aa"];
        Assert.Equal(12.5, first.CpuPercent);
        Assert.Equal(346, first.MemoryUsedMb);
        Assert.Equal(1024, first.MemoryLimitMb);

        var second = usage["craftpilot-134eb9b3"];
        Assert.Equal(0, second.CpuPercent);
        Assert.Equal(0, second.MemoryUsedMb);
        Assert.Equal(2048, second.MemoryLimitMb);
    }

    [Fact]
    public void Parse_ignores_lines_with_the_wrong_shape()
    {
        var usage = ServerStats.Parse(
            "no-pipes-here\n" +
            "|12%|1MiB / 1GiB\n" +
            "\n" +
            "craftpilot-096d46aa|not-a-percent|weird\n");

        var value = usage["craftpilot-096d46aa"];
        Assert.Null(value.CpuPercent);
        Assert.Null(value.MemoryUsedMb);
        Assert.Null(value.MemoryLimitMb);
    }

    [Fact]
    public void ParsePercent_accepts_only_real_percentages()
    {
        Assert.Equal(0, ServerStats.ParsePercent("0.00%"));
        Assert.Equal(12.5, ServerStats.ParsePercent("12.50%"));
        Assert.Equal(500, ServerStats.ParsePercent("500%"));
        Assert.Null(ServerStats.ParsePercent("-1%"));
        Assert.Null(ServerStats.ParsePercent("not-a-number"));
    }

    [Fact]
    public void ParseMemory_converts_to_mebibytes()
    {
        Assert.Equal((1L, 1024L), ServerStats.ParseMemory("512KiB / 1GiB"));
        Assert.Equal((1536L, 4096L), ServerStats.ParseMemory("1.5GiB / 4GiB"));
        Assert.Equal((0L, 1L), ServerStats.ParseMemory("1kB / 1MB"));
        Assert.Equal(((long?)null, (long?)null), ServerStats.ParseMemory("weird"));
    }

    [Fact]
    public void Parse_returns_empty_for_empty_output()
    {
        Assert.Empty(ServerStats.Parse(null));
        Assert.Empty(ServerStats.Parse(string.Empty));
        Assert.Empty(ServerStats.Parse("   \n"));
    }
}
