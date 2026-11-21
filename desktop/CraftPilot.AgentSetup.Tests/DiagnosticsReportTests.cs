using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class DiagnosticsReportTests
{
    [Fact]
    public void Redact_removes_claim_codes_tokens_and_bearer_headers()
    {
        var text = DiagnosticsReport.Redact(
            "claim_code=CP-ABCDE-12345\n" +
            "CRAFTPILOT_REGISTRATION_TOKEN=hemligt\n" +
            "Authorization: Bearer abc.def-123\n");

        Assert.DoesNotContain("CP-ABCDE-12345", text);
        Assert.Contains("CP-*****-*****", text);
        Assert.DoesNotContain("hemligt", text);
        Assert.DoesNotContain("abc.def-123", text);
    }

    [Fact]
    public void Build_contains_versions_and_redacts_the_agent_log()
    {
        var status = new StatusResponse
        {
            DockerOk = true,
            DockerState = DockerStateClassifier.Ready,
            DockerVersion = "29.5.2",
            MasterOk = true,
            MasterVersion = "1.4.0",
            MasterHttpStatus = 200,
            AgentRunning = true,
            Installed = true,
            AgentImage = "ghcr.io/cozilic/craftpilot-runtime-agent-rs:0.10",
            DataDir = "C:/CraftPilot/servers",
            InstallDir = "C:/Local/CraftPilot/Agent",
            Stats = new HostStats
            {
                DiskUsedBytes = 90,
                DiskTotalBytes = 100,
                ServersTotal = 1,
                ServersRunning = 1,
            },
            Servers =
            [
                new ServerContainer("craftpilot-096d46aa", "096d46aa-1111-2222-3333-444455556666", "running", "img")
                {
                    CpuPercent = 12.5,
                    MemoryUsedMb = 345,
                },
            ],
        };

        var text = DiagnosticsReport.Build(new DiagnosticsInput(
            "0.10.3",
            DateTimeOffset.Parse("2026-11-21T10:00:00+01:00"),
            "nod-1",
            "Windows 11",
            status,
            "claim_code=CP-ABCDE-12345\nnode_name=nod-1",
            "Name Command\nagent \"docker compose\"",
            "2.40.3"));

        Assert.Contains("Panel: 0.10.3", text);
        Assert.Contains("Version: 29.5.2", text);
        Assert.Contains("096d46aa-1111-2222-3333-444455556666", text);
        Assert.DoesNotContain("CP-ABCDE-12345", text);
        Assert.Contains("CP-*****-*****", text);
        Assert.Contains("node_name=nod-1", text);
    }
}
