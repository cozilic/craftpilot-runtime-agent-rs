using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class PanelNotifierTests
{
    private static readonly DateTimeOffset Now = new(2026, 11, 21, 10, 0, 0, TimeSpan.Zero);

    private static StatusResponse Healthy() => new()
    {
        DockerOk = true,
        DockerState = DockerStateClassifier.Ready,
        MasterOk = true,
        AgentRunning = true,
        Installed = true,
        Stats = new HostStats { DiskUsedBytes = 10, DiskTotalBytes = 100 },
    };

    [Fact]
    public void First_observation_primes_without_notifications()
    {
        var notifier = new PanelNotifier();
        var down = Healthy();
        down = new StatusResponse
        {
            DockerOk = false,
            DockerState = DockerStateClassifier.NotRunning,
            Installed = true,
        };

        Assert.Empty(notifier.Observe(down, null, Now));
    }

    [Fact]
    public void A_new_problem_is_reported_once_per_cooldown()
    {
        var notifier = new PanelNotifier();
        Assert.Empty(notifier.Observe(Healthy(), null, Now));

        var down = new StatusResponse
        {
            DockerOk = false,
            DockerState = DockerStateClassifier.NotRunning,
            Installed = true,
        };

        var first = notifier.Observe(down, null, Now.AddMinutes(1));
        Assert.Contains(first, alert => alert.Key == "docker");
        Assert.Empty(notifier.Observe(down, null, Now.AddMinutes(2)));
        Assert.Contains(
            notifier.Observe(down, null, Now.AddMinutes(11)),
            alert => alert.Key == "docker");
    }

    [Fact]
    public void Low_disk_and_a_crashed_server_produce_warnings()
    {
        var status = new StatusResponse
        {
            DockerOk = true,
            DockerState = DockerStateClassifier.Ready,
            MasterOk = true,
            AgentRunning = true,
            Installed = true,
            Stats = new HostStats
            {
                DiskUsedBytes = 96L * 1024 * 1024 * 1024,
                DiskTotalBytes = 100L * 1024 * 1024 * 1024,
            },
            Servers =
            [
                new ServerContainer("craftpilot-096d46aa", "096d46aa-1111-2222-3333-444455556666", "exited", "img", ExitCode: 1),
            ],
        };

        var alerts = PanelAlertEvaluator.Evaluate(status, null);
        Assert.Contains(alerts, alert => alert.Key == "disk");
        Assert.Contains(alerts, alert => alert.Key.StartsWith("server:", StringComparison.Ordinal));
    }

    [Fact]
    public void Update_available_is_a_warning()
    {
        var alerts = PanelAlertEvaluator.Evaluate(
            Healthy(),
            new UpdateCheckResponse { UpdateAvailable = true, LatestVersion = "0.11.0" });

        Assert.Contains(alerts, alert => alert.Key == "panel-update");
    }
}
