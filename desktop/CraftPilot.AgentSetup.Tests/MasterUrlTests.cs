using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class MasterUrlTests
{
    [Theory]
    [InlineData("https://craftpilot.se", "https://craftpilot.se")]
    [InlineData(" https://craftpilot.se/ ", "https://craftpilot.se")]
    [InlineData("HTTPS://CRAFTPILOT.SE", "https://craftpilot.se")]
    public void TryNormalize_accepts_the_official_master(string input, string expected)
    {
        Assert.True(MasterUrl.TryNormalize(input, out var normalized, out var error), error);
        Assert.Equal(expected, normalized);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("craftpilot.se")]
    [InlineData("http://craftpilot.se")]
    [InlineData("https://craftpilot.se/dashboard")]
    [InlineData("https://craftpilot.se?x=1")]
    [InlineData("https://user:pass@craftpilot.se")]
    [InlineData("https://craftpilot.se:8443")]
    [InlineData("https://www.craftpilot.se")]
    [InlineData("https://example.com")]
    public void TryNormalize_rejects_other_masters(string? input)
    {
        Assert.False(MasterUrl.TryNormalize(input, out _, out var error));
        Assert.NotEmpty(error);
    }
}
