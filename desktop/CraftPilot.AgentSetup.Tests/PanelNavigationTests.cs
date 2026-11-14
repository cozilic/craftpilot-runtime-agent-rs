using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class PanelNavigationTests
{
    private const string PanelUrl = "http://127.0.0.1:5188";

    [Theory]
    [InlineData("http://127.0.0.1:5188/")]
    [InlineData("http://127.0.0.1:5188/app.js")]
    [InlineData("http://127.0.0.1:5188/api/version")]
    public void IsLocalPanelUri_accepts_the_panel_origin(string value)
    {
        Assert.True(PanelNavigation.IsLocalPanelUri(value, PanelUrl));
    }

    [Theory]
    [InlineData("http://127.0.0.1:5189/")]
    [InlineData("https://github.com/cozilic/craftpilot-runtime-agent-rs")]
    [InlineData("file:///C:/Windows/System32/calc.exe")]
    [InlineData("javascript:alert(1)")]
    [InlineData(null)]
    public void IsLocalPanelUri_rejects_other_origins(string? value)
    {
        Assert.False(PanelNavigation.IsLocalPanelUri(value, PanelUrl));
    }

    [Theory]
    [InlineData("https://github.com/cozilic/craftpilot-runtime-agent-rs")]
    [InlineData("http://127.0.0.1:5189/")]
    public void IsSafeExternalUri_accepts_http_and_https(string value)
    {
        Assert.True(PanelNavigation.IsSafeExternalUri(value));
    }

    [Theory]
    [InlineData("file:///C:/Windows/System32/calc.exe")]
    [InlineData("javascript:alert(1)")]
    [InlineData(null)]
    public void IsSafeExternalUri_rejects_other_schemes(string? value)
    {
        Assert.False(PanelNavigation.IsSafeExternalUri(value));
    }
}
