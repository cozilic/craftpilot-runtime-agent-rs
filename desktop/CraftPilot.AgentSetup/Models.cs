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
    public string? AgentStatus { get; init; }
    public string? AgentImage { get; init; }
    public string? AgentError { get; init; }

    public bool Installed { get; init; }
    public bool EnrollmentCompleted { get; init; }
    public string? ClaimCode { get; init; }
    public string DataDir { get; init; } = string.Empty;
    public string InstallDir { get; init; } = string.Empty;
    public bool Busy { get; init; }
}

public sealed record OperationResponse(
    bool Ok,
    string Output,
    string? Error,
    StatusResponse? Status);

public sealed class UpdateCheckResponse
{
    public string CurrentVersion { get; init; } = "0.0.0";
    public string? LatestVersion { get; init; }
    public string? ReleaseUrl { get; init; }
    public bool UpdateAvailable { get; init; }
    public string? Error { get; init; }
    public DateTimeOffset CheckedAt { get; init; }
}
