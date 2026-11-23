namespace CraftPilot.AgentSetup;

public enum StartupAgentAction
{
    /// Vänta och fråga igen.
    Wait,
    StartAgent,
    Running,
    NotInstalled,
    /// Containern finns men kör inte; panelen startar den inte åt användaren.
    Stopped,
    DockerTimeout,
    AgentTimeout,
}

public enum StartupAgentOutcome
{
    Running,
    NotInstalled,
    Stopped,
    DockerTimeout,
    AgentTimeout,
    StartFailed,
}

/// <summary>
/// Vad panelens uppstartsvakt kom fram till. <c>Started</c> betyder att
/// panelen själv startade agenten, så trayen vet om en notis är relevant.
/// </summary>
public sealed record StartupAgentResult(StartupAgentOutcome Outcome, bool Started, string? Error)
{
    public bool Running => Outcome == StartupAgentOutcome.Running;

    /// <summary>Sant när något gick fel som användaren behöver se.</summary>
    public bool Failed =>
        Outcome is StartupAgentOutcome.DockerTimeout
            or StartupAgentOutcome.AgentTimeout
            or StartupAgentOutcome.StartFailed;
}

/// <summary>Det vakten behöver veta om ett statusprov.</summary>
public readonly record struct StartupAgentObservation(
    bool DockerOk,
    bool Installed,
    bool AgentExists,
    bool AgentRunning,
    bool Busy,
    bool StartAttempted,
    bool StoppedByUser,
    TimeSpan Elapsed,
    TimeSpan SinceStartAttempt,
    TimeSpan DockerGrace,
    TimeSpan AgentGrace);

/// <summary>
/// Besluten är rena och testbara: inget startas förrän Docker är redo, ingen
/// start när containern redan finns (då har någon stoppat den med flit), och
/// ett försök leder antingen till en körande agent eller till en tidsgräns.
/// </summary>
public static class StartupAgentPolicy
{
    public static StartupAgentAction Next(StartupAgentObservation observation)
    {
        if (observation.AgentRunning)
        {
            return StartupAgentAction.Running;
        }

        // En pågående åtgärd (installation, uppdatering, stopp) äger agenten.
        if (observation.Busy)
        {
            return StartupAgentAction.Wait;
        }

        if (!observation.DockerOk)
        {
            return observation.Elapsed < observation.DockerGrace
                ? StartupAgentAction.Wait
                : StartupAgentAction.DockerTimeout;
        }

        if (!observation.Installed)
        {
            return StartupAgentAction.NotInstalled;
        }

        // Användaren stoppade agenten i panelen; den startar inte igen för att
        // panelen råkar startas om.
        if (observation.StoppedByUser)
        {
            return StartupAgentAction.Stopped;
        }

        if (observation.StartAttempted)
        {
            return observation.SinceStartAttempt < observation.AgentGrace
                ? StartupAgentAction.Wait
                : StartupAgentAction.AgentTimeout;
        }

        // Containern finns men kör inte: den har stoppats av användaren eller
        // av ett tidigare fel, och panelen ska inte starta den bakom ryggen.
        if (observation.AgentExists)
        {
            return StartupAgentAction.Stopped;
        }

        return StartupAgentAction.StartAgent;
    }
}

/// <summary>
/// Ser vid panelstart till att agentens compose-projekt faktiskt kör. Efter en
/// omstart av datorn kan Docker Desktop behöva en stund, och en container som
/// aldrig skapades (t.ex. efter en misslyckad installation) kommer inte
/// tillbaka av sig själv — då startar panelen den. Kommer agenten inte upp
/// inom tidsgränsen blir det en notis i trayen i stället för en tyst nod.
/// </summary>
public sealed class StartupAgentWatchdog
{
    private readonly Func<CancellationToken, Task<StatusResponse>> _status;
    private readonly Func<CancellationToken, Task<OperationResponse>> _start;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;
    private readonly Func<DateTimeOffset> _clock;
    private readonly TimeSpan _dockerGrace;
    private readonly TimeSpan _agentGrace;
    private readonly TimeSpan _pollInterval;
    private readonly Func<bool> _stoppedByUser;

    public StartupAgentWatchdog(
        Func<CancellationToken, Task<StatusResponse>> status,
        Func<CancellationToken, Task<OperationResponse>> start,
        Func<TimeSpan, CancellationToken, Task>? delay = null,
        Func<DateTimeOffset>? clock = null,
        TimeSpan? dockerGrace = null,
        TimeSpan? agentGrace = null,
        TimeSpan? pollInterval = null,
        Func<bool>? stoppedByUser = null)
    {
        _status = status;
        _start = start;
        _delay = delay ?? Task.Delay;
        _clock = clock ?? (() => DateTimeOffset.UtcNow);
        _dockerGrace = dockerGrace ?? TimeSpan.FromMinutes(2);
        _agentGrace = agentGrace ?? TimeSpan.FromMinutes(3);
        _pollInterval = pollInterval ?? TimeSpan.FromSeconds(10);
        _stoppedByUser = stoppedByUser ?? (() => false);
    }

    public async Task<StartupAgentResult> RunAsync(CancellationToken ct = default)
    {
        var startedAt = _clock();
        DateTimeOffset? startAttemptAt = null;
        string? startError = null;

        while (true)
        {
            ct.ThrowIfCancellationRequested();

            var status = await _status(ct);
            var action = StartupAgentPolicy.Next(new StartupAgentObservation(
                DockerOk: status.DockerOk,
                Installed: status.Installed,
                AgentExists: status.AgentExists,
                AgentRunning: status.AgentRunning,
                Busy: status.Busy,
                StartAttempted: startAttemptAt is not null,
                StoppedByUser: _stoppedByUser(),
                Elapsed: _clock() - startedAt,
                SinceStartAttempt: startAttemptAt is { } attemptAt ? _clock() - attemptAt : TimeSpan.Zero,
                DockerGrace: _dockerGrace,
                AgentGrace: _agentGrace));

            switch (action)
            {
                case StartupAgentAction.Running:
                    return new StartupAgentResult(StartupAgentOutcome.Running, startAttemptAt is not null, startError);
                case StartupAgentAction.NotInstalled:
                    return new StartupAgentResult(StartupAgentOutcome.NotInstalled, false, null);
                case StartupAgentAction.Stopped:
                    return new StartupAgentResult(StartupAgentOutcome.Stopped, false, null);
                case StartupAgentAction.DockerTimeout:
                    return new StartupAgentResult(StartupAgentOutcome.DockerTimeout, false, null);
                case StartupAgentAction.AgentTimeout:
                    return new StartupAgentResult(
                        StartupAgentOutcome.AgentTimeout,
                        true,
                        startError ?? "Agenten kom inte upp i tid.");
                case StartupAgentAction.StartAgent:
                    startAttemptAt = _clock();
                    var started = await _start(ct);
                    if (!started.Ok)
                    {
                        startError = started.Error ?? "Kunde inte starta agenten.";
                        return new StartupAgentResult(StartupAgentOutcome.StartFailed, true, startError);
                    }

                    break;
                default:
                    await _delay(_pollInterval, ct);
                    break;
            }
        }
    }
}

/// <summary>Notisen trayen visar efter uppstartsvakten, null när inget behöver sägas.</summary>
public sealed record StartupAgentNotice(string Title, string Message, bool Error);

public static class StartupAgentNoticeFactory
{
    public static StartupAgentNotice? For(StartupAgentResult result) => result.Outcome switch
    {
        StartupAgentOutcome.Running when result.Started => new StartupAgentNotice(
            "Agenten startades",
            "Agentcontainern saknades vid panelstart och startades automatiskt. Noden anmäler sig igen om en stund.",
            false),
        StartupAgentOutcome.StartFailed => new StartupAgentNotice(
            "Kunde inte starta agenten",
            result.Error ?? "Panelen kunde inte starta agentcontainern.",
            true),
        StartupAgentOutcome.AgentTimeout => new StartupAgentNotice(
            "Agenten kom inte upp",
            "Agenten startades vid panelstart men kör fortfarande inte. Öppna panelen och loggen för att se varför.",
            true),
        StartupAgentOutcome.DockerTimeout => new StartupAgentNotice(
            "Docker är inte redo",
            "Panelen väntade på Docker men motorn blev inte redo. Starta Docker Desktop och öppna panelen igen.",
            true),
        _ => null,
    };
}
