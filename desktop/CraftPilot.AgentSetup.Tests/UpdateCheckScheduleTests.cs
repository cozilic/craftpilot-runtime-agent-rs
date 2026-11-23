using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

// Schemat för panelens versionskontroll och filen som sparar valet.
public sealed class UpdateCheckScheduleTests
{
    private static readonly DateTimeOffset Now = new(2026, 11, 20, 12, 0, 0, TimeSpan.Zero);

    [Theory]
    [InlineData("manual", UpdateCheckIntervals.Manual)]
    [InlineData("weekly", UpdateCheckIntervals.Weekly)]
    [InlineData("daily", UpdateCheckIntervals.Daily)]
    [InlineData(null, UpdateCheckIntervals.Daily)]
    [InlineData("", UpdateCheckIntervals.Daily)]
    [InlineData("varje timme", UpdateCheckIntervals.Daily)]
    public void Normalize_faller_tillbaka_pa_dagligen(string? value, string expected)
    {
        Assert.Equal(expected, UpdateCheckIntervals.Normalize(value));
    }

    [Fact]
    public void Manuell_kontroll_ar_aldrig_due()
    {
        Assert.False(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Manual, null, Now));
        Assert.False(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Manual, Now.AddDays(-30), Now));
    }

    [Fact]
    public void Daglig_kontroll_ar_due_forst_efter_ett_dygn()
    {
        Assert.False(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Daily, Now.AddHours(-23), Now));
        Assert.True(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Daily, Now.AddHours(-24), Now));
    }

    [Fact]
    public void Veckokontroll_ar_due_forst_efter_sju_dygn()
    {
        Assert.False(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Weekly, Now.AddDays(-6), Now));
        Assert.True(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Weekly, Now.AddDays(-7), Now));
    }

    [Fact]
    public void Utan_tidigare_kontroll_ar_automatiken_due()
    {
        Assert.True(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Daily, null, Now));
        Assert.True(UpdateCheckIntervals.IsDue(UpdateCheckIntervals.Weekly, null, Now));
    }

    [Fact]
    public void Store_sparar_intervallet_och_senaste_kontrollen()
    {
        var path = TempPath();
        try
        {
            var store = new PanelSettingsStore(path);

            // Utan fil gäller daglig kontroll, alltså panelens tidigare beteende.
            Assert.Equal(UpdateCheckIntervals.Daily, store.Load().UpdateCheckInterval);
            Assert.Null(store.Load().LastCheckedAt);

            store.SaveInterval(UpdateCheckIntervals.Weekly);
            var saved = store.SaveCheck(
                Now,
                "v0.10.3",
                "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/tag/v0.10.3");

            var reloaded = new PanelSettingsStore(path).Load();
            Assert.Equal(UpdateCheckIntervals.Weekly, reloaded.UpdateCheckInterval);
            Assert.Equal(Now, reloaded.LastCheckedAt);
            Assert.Equal("v0.10.3", reloaded.LatestVersion);
            Assert.Equal(saved.ReleaseUrl, reloaded.ReleaseUrl);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Agent_stopped_by_user_survives_a_restart_and_can_be_cleared()
    {
        var path = TempPath();
        try
        {
            var store = new PanelSettingsStore(path);
            Assert.False(store.Load().AgentStoppedByUser);

            store.SaveAgentStoppedByUser(true);
            Assert.True(new PanelSettingsStore(path).Load().AgentStoppedByUser);

            // Ett Starta nollställer markeringen, så nästa stopp kräver ett nytt val.
            store.SaveAgentStoppedByUser(false);
            Assert.False(new PanelSettingsStore(path).Load().AgentStoppedByUser);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Store_faller_tillbaka_pa_standard_nar_filen_ar_trasig()
    {
        var path = TempPath();
        File.WriteAllText(path, "{ inte json");
        try
        {
            var settings = new PanelSettingsStore(path).Load();

            Assert.Equal(UpdateCheckIntervals.Daily, settings.UpdateCheckInterval);
            Assert.Null(settings.LastCheckedAt);
            Assert.False(settings.AgentStoppedByUser);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Okant_intervall_i_filen_blir_dagligen()
    {
        var path = TempPath();
        File.WriteAllText(path, "{\"updateCheckInterval\":\"varje minut\"}");
        try
        {
            Assert.Equal(UpdateCheckIntervals.Daily, new PanelSettingsStore(path).Load().UpdateCheckInterval);
        }
        finally
        {
            File.Delete(path);
        }
    }

    private static string TempPath() => Path.Combine(
        Path.GetTempPath(),
        $"craftpilot-panel-settings-{Guid.NewGuid():N}.json");
}
