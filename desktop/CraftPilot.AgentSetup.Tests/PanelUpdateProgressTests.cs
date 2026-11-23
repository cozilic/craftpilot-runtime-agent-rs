using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class PanelUpdateProgressTests
{
    [Fact]
    public void Progress_starts_idle_without_percent()
    {
        var updater = new PanelUpdater();

        Assert.Equal(PanelUpdateStages.Idle, updater.Progress.Stage);
        Assert.False(updater.Progress.Active);
        Assert.Null(updater.Progress.Percent);
    }

    [Fact]
    public void Progress_carries_the_download_percent()
    {
        var progress = new PanelDownloadProgress(true, 50, 100) { Stage = PanelUpdateStages.Download };

        Assert.Equal(PanelUpdateStages.Download, progress.Stage);
        Assert.Equal(50, progress.Percent);
        Assert.Equal("idle", new PanelDownloadProgress(false, 0, -1).Stage);
    }

    [Fact]
    public async Task An_update_that_cannot_install_itself_resets_the_stage()
    {
        // Testerna körs inte som CraftPilot.AgentSetup.exe, så uppdateringen
        // avbryts direkt — men steget ska vara tillbaka på idle efteråt, annars
        // blir baren kvar i panelen.
        var updater = new PanelUpdater();

        var result = await updater.UpdateAsync();

        Assert.False(result.Ok);
        Assert.Equal(PanelUpdateStages.Idle, updater.Progress.Stage);
        Assert.False(updater.Progress.Active);
    }
}
