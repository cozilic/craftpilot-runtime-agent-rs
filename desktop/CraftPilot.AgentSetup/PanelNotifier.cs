using System.Globalization;

namespace CraftPilot.AgentSetup;

public enum PanelAlertKind
{
    Warning,
    Error,
}

public sealed record PanelAlert(
    string Key,
    string Title,
    string Message,
    PanelAlertKind Kind,
    TimeSpan Cooldown);

/// <summary>
/// Avgör vilka problem som är värda en notis. Logiken är ren och testbar;
/// cooldown och "första kontrollen" hanteras av <see cref="PanelNotifier"/>.
/// </summary>
public static class PanelAlertEvaluator
{
    public static IReadOnlyList<PanelAlert> Evaluate(
        StatusResponse status,
        UpdateCheckResponse? version)
    {
        var alerts = new List<PanelAlert>();

        if (!status.DockerOk)
        {
            alerts.Add(new PanelAlert(
                "docker",
                "Docker är inte redo",
                status.DockerState == DockerStateClassifier.NotRunning
                    ? "Docker Desktop är installerat men motorn är inte igång."
                    : "Panelen får ingen kontakt med Docker. Servrarna kan inte startas eller övervakas.",
                PanelAlertKind.Error,
                TimeSpan.FromMinutes(10)));
        }

        if (status.DockerOk && status.Installed && !status.AgentRunning)
        {
            alerts.Add(new PanelAlert(
                "agent",
                "Agenten är stoppad",
                "Agentcontainern kör inte. Noden syns offline i panelen tills den startar igen.",
                PanelAlertKind.Error,
                TimeSpan.FromMinutes(15)));
        }

        if (!status.MasterOk)
        {
            alerts.Add(new PanelAlert(
                "master",
                "Mastern svarar inte",
                status.MasterError is { Length: > 0 } error
                    ? $"Panelen når inte mastern: {error}"
                    : "Panelen når inte mastern. Kontrollera nätverket och att craftpilot.se är tillgängligt.",
                PanelAlertKind.Error,
                TimeSpan.FromMinutes(30)));
        }

        var (freeDisk, diskTotal) = DiskSpace(status.Stats);
        if (diskTotal > 0)
        {
            // Både en relativ och en absolut gräns: 10 % av en liten disk kan
            // vara för lite för en värld, och 5 GiB på en stor disk likaså.
            var threshold = Math.Max(5L * 1024 * 1024 * 1024, diskTotal / 10);
            if (freeDisk < threshold)
            {
                alerts.Add(new PanelAlert(
                    "disk",
                    "Disken börjar bli full",
                    string.Format(
                        CultureInfo.InvariantCulture,
                        "Det finns {0:0.0} GB kvar på serverdata-disken. Minecraft kan inte spara världen när disken är full.",
                        freeDisk / 1024.0 / 1024 / 1024),
                    PanelAlertKind.Warning,
                    TimeSpan.FromHours(6)));
            }
        }

        foreach (var server in status.Servers)
        {
            if (!server.Running && server.ExitCode is > 0)
            {
                alerts.Add(new PanelAlert(
                    $"server:{server.ServerId}:{server.ExitCode}",
                    "En server kraschade",
                    $"Server {server.ShortId} avslutades med kod {server.ExitCode}. Öppna servern i panelen för att se konsolen.",
                    PanelAlertKind.Warning,
                    TimeSpan.FromHours(1)));
            }
        }

        if (version?.UpdateAvailable == true && !string.IsNullOrWhiteSpace(version.LatestVersion))
        {
            alerts.Add(new PanelAlert(
                "panel-update",
                "Ny panelversion finns",
                $"Panel {version.LatestVersion} är tillgänglig. Öppna panelen för att uppdatera.",
                PanelAlertKind.Warning,
                TimeSpan.FromHours(24)));
        }

        return alerts;
    }

    private static (long Free, long Total) DiskSpace(HostStats stats) =>
        stats.DiskTotalBytes <= 0
            ? (0, 0)
            : (Math.Max(0, stats.DiskTotalBytes - stats.DiskUsedBytes), stats.DiskTotalBytes);
}

/// <summary>
/// Skickar högst en notis per nyckel och cooldown. Första kontrollen efter
/// panelstart primer bara läget, så en panel som startar med ett redan känt
/// fel inte fyller skärmen med ballonger.
/// </summary>
public sealed class PanelNotifier
{
    private readonly Dictionary<string, DateTimeOffset> _lastShown = new(StringComparer.Ordinal);
    private bool _primed;

    public IReadOnlyList<PanelAlert> Observe(
        StatusResponse status,
        UpdateCheckResponse? version,
        DateTimeOffset now)
    {
        var alerts = PanelAlertEvaluator.Evaluate(status, version);
        if (!_primed)
        {
            _primed = true;
            foreach (var alert in alerts)
            {
                _lastShown[alert.Key] = now;
            }
            return [];
        }

        var due = new List<PanelAlert>();
        foreach (var alert in alerts)
        {
            if (_lastShown.TryGetValue(alert.Key, out var shownAt) && now - shownAt < alert.Cooldown)
            {
                continue;
            }

            _lastShown[alert.Key] = now;
            due.Add(alert);
        }

        return due;
    }
}
