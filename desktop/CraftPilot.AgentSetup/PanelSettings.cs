using System.Text.Json;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Hur ofta panelen själv letar efter en ny version. Valet sparas i panelens
/// inställningsfil, så det gäller även efter en omstart.
/// </summary>
public static class UpdateCheckIntervals
{
    public const string Manual = "manual";
    public const string Daily = "daily";
    public const string Weekly = "weekly";

    /// <summary>Tomt eller okänt värde blir "daily" — samma kontroll som panelen alltid haft.</summary>
    public static string Normalize(string? value) =>
        value is Manual or Daily or Weekly ? value! : Daily;

    /// <summary>Hur länge en kontroll gäller. null = panelen kontrollerar bara manuellt.</summary>
    public static TimeSpan? Window(string? value) => Normalize(value) switch
    {
        Manual => null,
        Weekly => TimeSpan.FromDays(7),
        _ => TimeSpan.FromDays(1),
    };

    /// <summary>Sant när en automatisk kontroll ska göras nu.</summary>
    public static bool IsDue(string? value, DateTimeOffset? lastCheckedAt, DateTimeOffset now)
    {
        var window = Window(value);
        if (window is null)
        {
            return false;
        }

        return lastCheckedAt is null || now - lastCheckedAt.Value >= window.Value;
    }
}

/// <summary>
/// Panelens egna inställningar och den senaste versionskontrollen, sparade i
/// %LOCALAPPDATA%\CraftPilot\panel-settings.json. Filen är liten och skrivs
/// bara när något ändras; går den inte att läsa gäller standardvärdet, så en
/// trasig fil aldrig stoppar panelen.
/// </summary>
public sealed class PanelSettingsStore
{
    public sealed record Snapshot(
        string UpdateCheckInterval,
        DateTimeOffset? LastCheckedAt,
        string? LatestVersion,
        string? ReleaseUrl)
    {
        /// <summary>
        /// Användaren stoppade agenten i panelen; uppstartsvakten ska inte
        /// starta den igen. Saknas fältet i filen är svaret nej.
        /// </summary>
        public bool AgentStoppedByUser { get; init; }

        public static Snapshot Default { get; } =
            new(UpdateCheckIntervals.Daily, null, null, null);
    }

    private static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        WriteIndented = true,
    };

    private readonly string _path;
    private readonly object _gate = new();

    public PanelSettingsStore() : this(DefaultPath())
    {
    }

    public PanelSettingsStore(string path)
    {
        _path = path;
    }

    public static string DefaultPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "CraftPilot",
        "panel-settings.json");

    public Snapshot Load()
    {
        lock (_gate)
        {
            return Read();
        }
    }

    /// <summary>Sparar det valda intervallet och returnerar hela läget.</summary>
    public Snapshot SaveInterval(string? interval)
    {
        lock (_gate)
        {
            var snapshot = Read() with
            {
                UpdateCheckInterval = UpdateCheckIntervals.Normalize(interval),
            };
            Write(snapshot);
            return snapshot;
        }
    }

    /// <summary>
    /// Kom ihåg att agenten stoppats med flit, så uppstartsvakten inte startar
    /// den igen nästa gång panelen startar. Ett vanligt Starta nollställer.
    /// </summary>
    public Snapshot SaveAgentStoppedByUser(bool stopped)
    {
        lock (_gate)
        {
            var snapshot = Read() with { AgentStoppedByUser = stopped };
            Write(snapshot);
            return snapshot;
        }
    }

    /// <summary>
    /// Sparar versionen som senast sågs, så panelen kan visa samma svar efter
    /// en omstart utan att fråga GitHub igen.
    /// </summary>
    public Snapshot SaveCheck(DateTimeOffset checkedAt, string latestVersion, string? releaseUrl)
    {
        lock (_gate)
        {
            var snapshot = Read() with
            {
                LastCheckedAt = checkedAt,
                LatestVersion = latestVersion,
                ReleaseUrl = releaseUrl,
            };
            Write(snapshot);
            return snapshot;
        }
    }

    private Snapshot Read()
    {
        try
        {
            if (!File.Exists(_path))
            {
                return Snapshot.Default;
            }

            var stored = JsonSerializer.Deserialize<Snapshot>(File.ReadAllText(_path), Json);
            if (stored is null)
            {
                return Snapshot.Default;
            }

            return stored with
            {
                UpdateCheckInterval = UpdateCheckIntervals.Normalize(stored.UpdateCheckInterval),
            };
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            return Snapshot.Default;
        }
    }

    private void Write(Snapshot snapshot)
    {
        try
        {
            var folder = Path.GetDirectoryName(_path);
            if (!string.IsNullOrEmpty(folder))
            {
                Directory.CreateDirectory(folder);
            }

            File.WriteAllText(_path, JsonSerializer.Serialize(snapshot, Json));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Inställningen gäller då bara till nästa omstart.
        }
    }
}
