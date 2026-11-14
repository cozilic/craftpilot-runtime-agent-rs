using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class ReleaseVersionTests
{
    [Theory]
    [InlineData("v0.2.0", 0, 2, 0, 0)]
    [InlineData("1.2", 1, 2, 0, 0)]
    [InlineData("v1.2.3.4", 1, 2, 3, 4)]
    [InlineData("v1.2.3-beta.1", 1, 2, 3, 0)]
    [InlineData("v1.2.3+build.7", 1, 2, 3, 0)]
    public void TryParse_reads_release_versions(
        string value,
        int major,
        int minor,
        int build,
        int revision)
    {
        Assert.True(ReleaseVersion.TryParse(value, out var version));
        Assert.Equal(new ReleaseVersion(major, minor, build, revision), version);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("v")]
    [InlineData("latest")]
    [InlineData("1.2.3.4.5")]
    public void TryParse_rejects_invalid_versions(string? value)
    {
        Assert.False(ReleaseVersion.TryParse(value, out _));
    }

    [Theory]
    [InlineData("v0.2.0", "0.1.0", true)]
    [InlineData("0.2.0", "v0.1.0", true)]
    [InlineData("v0.1.1", "0.1.0", true)]
    [InlineData("v1.0", "0.9.9", true)]
    [InlineData("v0.1.0.1", "0.1.0", true)]
    [InlineData("v0.1.0", "0.1.0", false)]
    [InlineData("v0.1.0", "0.1.0.0", false)]
    [InlineData("v0.1.0-beta.1", "0.1.0", false)]
    [InlineData("nonsense", "0.1.0", false)]
    [InlineData("v0.1.0", "nonsense", false)]
    public void IsNewer_compares_release_versions(string? latest, string? current, bool expected)
    {
        Assert.Equal(expected, ReleaseVersion.IsNewer(latest, current));
    }

    [Fact]
    public void PanelVersion_is_a_parseable_release_version()
    {
        Assert.True(ReleaseVersion.TryParse(PanelVersion.Current, out _));
    }
}
