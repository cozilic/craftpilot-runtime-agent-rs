namespace CraftPilot.AgentSetup.Tests;

// Felet vi råkade ut för: agentcontainern fanns inte alls, SFTPGo puttrade på
// och ingen märkte att noden var tyst. Testerna låser uppstartsvakten: vänta in
// Docker, starta bara när containern saknas, och larma när den inte kommer upp.
public sealed class StartupAgentWatchdogTests
{
    private static readonly TimeSpan DockerGrace = TimeSpan.FromMinutes(2);
    private static readonly TimeSpan AgentGrace = TimeSpan.FromMinutes(3);
    private static readonly TimeSpan Poll = TimeSpan.FromSeconds(10);

    [Fact]
    public async Task Starts_a_missing_container_and_reports_it_running()
    {
        var panel = new FakePanel(
            Status(dockerOk: true, installed: true, exists: false, running: false),
            Status(dockerOk: true, installed: true, exists: true, running: true));

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.Running, result.Outcome);
        Assert.True(result.Started);
        Assert.Equal(1, panel.StartCalls);
    }

    [Fact]
    public async Task Does_not_start_when_the_agent_is_already_running()
    {
        var panel = new FakePanel(Status(dockerOk: true, installed: true, exists: true, running: true));

        var result = await panel.RunAsync();

        Assert.True(result.Running);
        Assert.False(result.Started);
        Assert.Equal(0, panel.StartCalls);
        Assert.Empty(panel.Delays);
    }

    [Fact]
    public async Task Leaves_a_stopped_container_alone()
    {
        // Containern finns men kör inte: någon har stoppat den med flit, så
        // panelen ska inte starta den bakom ryggen på användaren.
        var panel = new FakePanel(Status(dockerOk: true, installed: true, exists: true, running: false));

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.Stopped, result.Outcome);
        Assert.False(result.Started);
        Assert.Equal(0, panel.StartCalls);
        Assert.Null(StartupAgentNoticeFactory.For(result));
    }

    [Fact]
    public async Task Does_not_restart_an_agent_the_user_stopped()
    {
        // Stoppa i panelen tar bort containern. Utan markeringen skulle nästa
        // panelstart se "containern saknas" och starta den igen.
        var panel = new FakePanel(Status(dockerOk: true, installed: true, exists: false, running: false))
        {
            StoppedByUser = true,
        };

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.Stopped, result.Outcome);
        Assert.Equal(0, panel.StartCalls);
        Assert.Null(StartupAgentNoticeFactory.For(result));
    }

    [Fact]
    public void A_stopped_by_user_marker_wins_over_a_missing_container()
    {
        var action = StartupAgentPolicy.Next(new StartupAgentObservation(
            DockerOk: true,
            Installed: true,
            AgentExists: false,
            AgentRunning: false,
            Busy: false,
            StartAttempted: false,
            StoppedByUser: true,
            Elapsed: TimeSpan.FromMinutes(1),
            SinceStartAttempt: TimeSpan.Zero,
            DockerGrace: DockerGrace,
            AgentGrace: AgentGrace));

        Assert.Equal(StartupAgentAction.Stopped, action);
    }

    [Fact]
    public async Task Does_nothing_when_the_agent_is_not_installed()
    {
        var panel = new FakePanel(Status(dockerOk: true, installed: false, exists: false, running: false));

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.NotInstalled, result.Outcome);
        Assert.Equal(0, panel.StartCalls);
        Assert.Null(StartupAgentNoticeFactory.For(result));
    }

    [Fact]
    public async Task Waits_for_docker_before_starting()
    {
        var panel = new FakePanel(
            Status(dockerOk: false, installed: true, exists: false, running: false),
            Status(dockerOk: false, installed: true, exists: false, running: false),
            Status(dockerOk: true, installed: true, exists: false, running: false),
            Status(dockerOk: true, installed: true, exists: true, running: true));

        var result = await panel.RunAsync();

        Assert.True(result.Running);
        Assert.Equal(1, panel.StartCalls);
        Assert.Equal(2, panel.Delays.Count);
        Assert.All(panel.Delays, delay => Assert.Equal(Poll, delay));
    }

    [Fact]
    public async Task Gives_up_when_docker_never_becomes_ready()
    {
        var panel = new FakePanel(Status(dockerOk: false, installed: true, exists: false, running: false));

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.DockerTimeout, result.Outcome);
        Assert.True(result.Failed);
        Assert.Equal(0, panel.StartCalls);
        Assert.Equal((int)(DockerGrace / Poll), panel.Delays.Count);
        Assert.Contains("Docker", StartupAgentNoticeFactory.For(result)!.Title);
    }

    [Fact]
    public async Task Gives_up_when_a_started_agent_never_runs()
    {
        var panel = new FakePanel(Status(dockerOk: true, installed: true, exists: false, running: false));

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.AgentTimeout, result.Outcome);
        Assert.True(result.Started);
        Assert.True(result.Failed);
        Assert.Equal(1, panel.StartCalls);
        Assert.Equal((int)(AgentGrace / Poll), panel.Delays.Count);
        Assert.True(StartupAgentNoticeFactory.For(result)!.Error);
    }

    [Fact]
    public async Task Reports_a_start_that_fails()
    {
        var panel = new FakePanel(Status(dockerOk: true, installed: true, exists: false, running: false))
        {
            StartResult = new OperationResponse(false, "", "compose up misslyckades", null),
        };

        var result = await panel.RunAsync();

        Assert.Equal(StartupAgentOutcome.StartFailed, result.Outcome);
        Assert.Equal("compose up misslyckades", result.Error);
        Assert.Equal(1, panel.StartCalls);
        var notice = StartupAgentNoticeFactory.For(result);
        Assert.NotNull(notice);
        Assert.True(notice.Error);
        Assert.Contains("compose up misslyckades", notice.Message);
    }

    [Fact]
    public async Task Waits_while_another_operation_owns_the_agent()
    {
        var panel = new FakePanel(
            Status(dockerOk: true, installed: true, exists: false, running: false, busy: true),
            Status(dockerOk: true, installed: true, exists: false, running: false, busy: true),
            Status(dockerOk: true, installed: true, exists: true, running: true));

        var result = await panel.RunAsync();

        Assert.True(result.Running);
        Assert.Equal(0, panel.StartCalls);
        Assert.Equal(2, panel.Delays.Count);
    }

    [Fact]
    public void Auto_start_is_reported_as_information_not_an_error()
    {
        var notice = StartupAgentNoticeFactory.For(new StartupAgentResult(StartupAgentOutcome.Running, true, null));

        Assert.NotNull(notice);
        Assert.False(notice.Error);
        Assert.Equal("Agenten startades", notice.Title);
    }

    [Fact]
    public void A_running_agent_the_panel_did_not_start_has_no_notice()
    {
        Assert.Null(StartupAgentNoticeFactory.For(new StartupAgentResult(StartupAgentOutcome.Running, false, null)));
    }

    [Theory]
    [InlineData(119, StartupAgentAction.Wait)]
    [InlineData(120, StartupAgentAction.DockerTimeout)]
    public void Docker_grace_is_a_hard_limit(int elapsedSeconds, StartupAgentAction expected)
    {
        var action = StartupAgentPolicy.Next(new StartupAgentObservation(
            DockerOk: false,
            Installed: true,
            AgentExists: false,
            AgentRunning: false,
            Busy: false,
            StartAttempted: false,
            StoppedByUser: false,
            Elapsed: TimeSpan.FromSeconds(elapsedSeconds),
            SinceStartAttempt: TimeSpan.Zero,
            DockerGrace: DockerGrace,
            AgentGrace: AgentGrace));

        Assert.Equal(expected, action);
    }

    [Theory]
    [InlineData(179, StartupAgentAction.Wait)]
    [InlineData(180, StartupAgentAction.AgentTimeout)]
    public void Agent_grace_is_a_hard_limit(int sinceStartSeconds, StartupAgentAction expected)
    {
        var action = StartupAgentPolicy.Next(new StartupAgentObservation(
            DockerOk: true,
            Installed: true,
            AgentExists: false,
            AgentRunning: false,
            Busy: false,
            StartAttempted: true,
            StoppedByUser: false,
            Elapsed: TimeSpan.FromMinutes(10),
            SinceStartAttempt: TimeSpan.FromSeconds(sinceStartSeconds),
            DockerGrace: DockerGrace,
            AgentGrace: AgentGrace));

        Assert.Equal(expected, action);
    }

    [Fact]
    public void Stopping_and_starting_the_agent_updates_the_marker()
    {
        // Markeringen sätts bara när stoppet lyckats och nollställs av varje väg
        // som startar agenten (installation, Starta, Uppdatera, Ta bort).
        var source = PanelSource("AgentSetupService.cs");

        Assert.Equal(1, source.Split("_settings.SaveAgentStoppedByUser(true);").Length - 1);
        Assert.True(source.Split("_settings.SaveAgentStoppedByUser(false);").Length - 1 >= 3);
    }

    [Fact]
    public void Tray_runs_the_watchdog_at_startup()
    {
        var source = PanelSource("TrayContext.cs");

        Assert.Contains("StartupAgentWatchdog", source);
        Assert.Contains("EnsureAgentStartedAsync()", source);
        Assert.Contains("StartupAgentNoticeFactory.For(result)", source);
        Assert.Contains("ShowBalloonTip", source);
    }

    private static StatusResponse Status(
        bool dockerOk,
        bool installed,
        bool exists,
        bool running,
        bool busy = false) => new()
    {
        DockerOk = dockerOk,
        Installed = installed,
        AgentExists = exists,
        AgentRunning = running,
        Busy = busy,
    };

    private static string PanelSource(string name)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            var candidate = Path.Combine(directory.FullName, "desktop", "CraftPilot.AgentSetup", name);
            if (File.Exists(candidate))
            {
                return File.ReadAllText(candidate);
            }

            directory = directory.Parent;
        }

        throw new FileNotFoundException(name);
    }

    private sealed class FakePanel
    {
        private readonly Queue<StatusResponse> _statuses;
        private StatusResponse _last;

        public FakePanel(params StatusResponse[] statuses)
        {
            _statuses = new Queue<StatusResponse>(statuses);
            _last = statuses[^1];
        }

        public int StartCalls { get; private set; }

        public OperationResponse StartResult { get; init; } = new(true, "startad", null, null);

        /// <summary>Användaren har stoppat agenten i panelen.</summary>
        public bool StoppedByUser { get; set; }

        public List<TimeSpan> Delays { get; } = [];

        public DateTimeOffset Now { get; private set; } = new(2026, 11, 24, 8, 0, 0, TimeSpan.Zero);

        public Task<StartupAgentResult> RunAsync()
        {
            var watchdog = new StartupAgentWatchdog(
                StatusAsync,
                StartAsync,
                DelayAsync,
                () => Now,
                DockerGrace,
                AgentGrace,
                Poll,
                () => StoppedByUser);

            return watchdog.RunAsync();
        }

        private Task<StatusResponse> StatusAsync(CancellationToken ct)
        {
            if (_statuses.Count > 0)
            {
                _last = _statuses.Dequeue();
            }

            return Task.FromResult(_last);
        }

        private Task<OperationResponse> StartAsync(CancellationToken ct)
        {
            StartCalls++;
            return Task.FromResult(StartResult);
        }

        private Task DelayAsync(TimeSpan delay, CancellationToken ct)
        {
            Delays.Add(delay);
            Now += delay;
            return Task.CompletedTask;
        }
    }
}
