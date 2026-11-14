namespace CraftPilot.AgentSetup;

public enum TrayState
{
    Busy,
    Ok,
    Warn,
    Bad,
}

public sealed record TrayStatus(TrayState State, string Label, string Tooltip);

public static class TrayStatusClassifier
{
    public static TrayStatus From(StatusResponse status)
    {
        if (status.Busy)
        {
            return new(TrayState.Busy, "Arbetar…", "CraftPilot: arbetar…");
        }

        if (!status.DockerOk)
        {
            return new(TrayState.Bad, "Docker är inte redo", "CraftPilot: Docker är inte redo");
        }

        if (!status.MasterOk)
        {
            return new(TrayState.Bad, "Mastern svarar inte", "CraftPilot: mastern svarar inte");
        }

        if (status.AgentRunning)
        {
            return new(TrayState.Ok, "Agenten är igång", "CraftPilot: agenten är igång");
        }

        if (status.Installed)
        {
            return new(TrayState.Warn, "Agenten är stoppad", "CraftPilot: agenten är stoppad");
        }

        return new(TrayState.Warn, "Ingen agent installerad", "CraftPilot: ingen agent installerad");
    }
}
