namespace CraftPilot.AgentSetup;

public sealed class StatusResponse
{
    public bool DockerOk { get; init; }
    public string DockerState { get; init; } = DockerStateClassifier.Error;
    public bool DockerDesktopInstalled { get; init; }
    public string? DockerVersion { get; init; }
    public string? DockerError { get; init; }

    public bool MasterOk { get; init; }
    public int? MasterHttpStatus { get; init; }
    public string? MasterVersion { get; init; }
    public long? MasterLatencyMs { get; init; }
    public string? MasterError { get; init; }
    public string MasterUrl { get; init; } = global::CraftPilot.AgentSetup.MasterUrl.Default;

    public bool AgentRunning { get; init; }
    /// Sant när en container med panelens namn finns, även om panelen själv
    /// saknar compose/env (t.ex. efter en installation med ett annat verktyg).
    public bool AgentExists { get; init; }
    public string? AgentStatus { get; init; }
    public string? AgentImage { get; init; }
    public string? AgentError { get; init; }

    public bool Installed { get; init; }
    public bool EnrollmentCompleted { get; init; }
    public string? ClaimCode { get; init; }
    /// <summary>Nodens namn i panelen (från agentloggen), null tills agenten rapporterat det.</summary>
    public string? NodeName { get; init; }
    /// <summary>Maskinens hostname — gör flera agenter på olika datorer åtskiljbara.</summary>
    public string HostName { get; init; } = Environment.MachineName;
    public string DataDir { get; init; } = string.Empty;
    public string InstallDir { get; init; } = string.Empty;
    public bool StartupEnabled { get; init; }
    public bool Busy { get; init; }

    /// <summary>Nodens resursläge, sammanställt lokalt (Docker + värden).</summary>
    public HostStats Stats { get; init; } = new();

    /// Servercontainrar som agenten kör på den här datorn, plus servrar som
    /// finns i serverdata-mappen men ännu inte startats.
    public IReadOnlyList<ServerContainer> Servers { get; init; } = [];
}

/// <summary>
/// Statistik för agentpanelens översikt. Allt hämtas på datorn — inget
/// kräver masterns API: Docker-containrarna, serverdata-mappen och Windows
/// egna räknare.
/// </summary>
public sealed record HostStats
{
    public int ServersTotal { get; init; }
    public int ServersRunning { get; init; }
    public bool PlayersKnown { get; init; }
    public int PlayersOnline { get; init; }
    public int? PlayersMax { get; init; }
    public long MemoryUsedBytes { get; init; }
    public long MemoryTotalBytes { get; init; }
    public long DiskUsedBytes { get; init; }
    public long DiskTotalBytes { get; init; }
    public double NetworkDownBps { get; init; }
    public double NetworkUpBps { get; init; }

    public static HostStats Empty { get; } = new();
}

public sealed record OperationResponse(
    bool Ok,
    string Output,
    string? Error,
    StatusResponse? Status);

/// <summary>
/// Svar från panelens versionskontroll. `NextCheckAt` är när nästa
/// automatiska kontroll är due — null betyder att panelen bara kontrollerar
/// manuellt.
/// </summary>
public sealed record UpdateCheckResponse
{
    public string CurrentVersion { get; init; } = "0.0.0";
    public string? LatestVersion { get; init; }
    public string? ReleaseUrl { get; init; }
    public bool UpdateAvailable { get; init; }
    public string? Error { get; init; }
    public DateTimeOffset? CheckedAt { get; init; }
    public string UpdateCheckInterval { get; init; } = UpdateCheckIntervals.Daily;
    public DateTimeOffset? NextCheckAt { get; init; }
}

/// Svar från panelens självuppdatering: nedladdning, kontrollsumma och
/// utbyte av panelens egen exe.
public sealed record PanelUpdateResult(
    bool Ok,
    string? Error,
    string? Version,
    string? ReleaseUrl);

/// Nedladdningens läge för panelens exe, så baren kan visas i procent i
/// stället för som en obestämd glidning.
public sealed record PanelDownloadProgress(bool Active, long Received, long Total)
{
    /// <summary>0–100 när servern skickade Content-Length, annars null.</summary>
    public int? Percent => Total > 0
        ? (int)Math.Clamp(Received * 100 / Total, 0, 100)
        : null;
}
