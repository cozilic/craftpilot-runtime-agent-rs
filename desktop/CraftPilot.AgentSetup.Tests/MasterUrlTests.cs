using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class MasterUrlTests
{
    [Theory]
    [InlineData("https://craftpilot.se", "https://craftpilot.se")]
    [InlineData(" https://craftpilot.se/ ", "https://craftpilot.se")]
    [InlineData("https://craftpilot.se:8443", "https://craftpilot.se:8443")]
    public void TryNormalize_accepts_https_origins(string input, string expected)
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
    public void TryNormalize_rejects_non_origin_values(string? input)
    {
        Assert.False(MasterUrl.TryNormalize(input, out _, out var error));
        Assert.NotEmpty(error);
    }
}
