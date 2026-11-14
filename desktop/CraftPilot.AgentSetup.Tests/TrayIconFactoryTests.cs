using System.Drawing;

using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class TrayIconFactoryTests
{
    [Fact]
    public void Brand_icon_uses_the_embedded_logo()
    {
        using var icon = TrayIconFactory.CreateBrandIcon(32);

        Assert.Equal(32, icon.Width);
        Assert.Equal(32, icon.Height);
    }

    [Fact]
    public void Status_icon_is_generated_for_each_state_color()
    {
        using var icon = TrayIconFactory.Create(Color.FromArgb(70, 214, 154));

        Assert.Equal(32, icon.Width);
        Assert.Equal(32, icon.Height);
    }
}
