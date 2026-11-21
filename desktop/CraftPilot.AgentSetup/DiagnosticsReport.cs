using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup;

/// <summary>Underlaget för en diagnosfil. Allt är redan insamlat av panelen.</summary>
public sealed record DiagnosticsInput(
    string PanelVersion,
    DateTimeOffset GeneratedAt,
    string HostName,
    string OsDescription,
    StatusResponse Status,
    string AgentLog,
    string? ComposePs,
    string? ComposeVersion);

/// <summary>
/// Bygger en textfil som en nodägare kan skicka med en supportfråga: versioner,
/// Docker-läge, nodens servrar och agentloggens svans. Claim-koder, tokens och
/// lösenord redigeras innan texten lämnar panelen.
/// </summary>
public static class DiagnosticsReport
{
    private static readonly Regex ClaimCode = new(
        @"\bCP-[A-Z0-9]{5}-[A-Z0-9]{5}\b",
        RegexOptions.CultureInvariant);

    private static readonly Regex SecretAssignment = new(
        @"(?im)^(\s*[A-Za-z0-9_]*(?:TOKEN|PASSWORD|SECRET|CREDENTIAL)[A-Za-z0-9_]*\s*=\s*).*$",
        RegexOptions.CultureInvariant);

    private static readonly Regex Bearer = new(
        @"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+",
        RegexOptions.CultureInvariant);

    public static string Redact(string? text)
    {
        var value = text ?? string.Empty;
        value = ClaimCode.Replace(value, "CP-*****-*****");
        value = SecretAssignment.Replace(value, "$1***");
        value = Bearer.Replace(value, "$1***");
        return value;
    }

    public static string Build(DiagnosticsInput input)
    {
        var status = input.Status;
        var stats = status.Stats;
        var text = new StringBuilder();

        text.AppendLine("CraftPilot Runtime Agent – diagnos");
        text.AppendLine($"Skapad: {input.GeneratedAt:yyyy-MM-dd HH:mm:ss zzz}");
        text.AppendLine($"Panel: {input.PanelVersion}");
        text.AppendLine($"Dator: {input.HostName}");
        text.AppendLine($"OS: {input.OsDescription}");
        text.AppendLine();

        text.AppendLine("[Docker]");
        text.AppendLine($"Status: {status.DockerState}");
        text.AppendLine($"Version: {status.DockerVersion ?? "—"}");
        text.AppendLine($"Compose: {input.ComposeVersion ?? "—"}");
        if (!string.IsNullOrWhiteSpace(status.DockerError))
        {
            text.AppendLine($"Fel: {status.DockerError}");
        }
        text.AppendLine();

        text.AppendLine("[Master]");
        text.AppendLine($"Adress: {status.MasterUrl}");
        text.AppendLine($"Svarar: {(status.MasterOk ? "ja" : "nej")} (HTTP {status.MasterHttpStatus?.ToString(CultureInfo.InvariantCulture) ?? "—"})");
        text.AppendLine($"Version: {status.MasterVersion ?? "—"}");
        text.AppendLine($"Latens: {status.MasterLatencyMs?.ToString(CultureInfo.InvariantCulture) ?? "—"} ms");
        if (!string.IsNullOrWhiteSpace(status.MasterError))
        {
            text.AppendLine($"Fel: {status.MasterError}");
        }
        text.AppendLine();

        text.AppendLine("[Agent]");
        text.AppendLine($"Installerad: {(status.Installed ? "ja" : "nej")}");
        text.AppendLine($"Container: {status.AgentStatus ?? "—"}");
        text.AppendLine($"Image: {status.AgentImage ?? "—"}");
        text.AppendLine($"Ansluten: {(status.EnrollmentCompleted ? "ja" : "nej")}");
        text.AppendLine($"Nodnamn: {status.NodeName ?? "—"}");
        if (!string.IsNullOrWhiteSpace(status.AgentError))
        {
            text.AppendLine($"Fel: {status.AgentError}");
        }
        text.AppendLine();

        text.AppendLine("[Värden]");
        text.AppendLine($"Serverdata: {status.DataDir}");
        text.AppendLine($"Installationsfiler: {status.InstallDir}");
        text.AppendLine($"Minne: {stats.MemoryUsedBytes} / {stats.MemoryTotalBytes} byte");
        text.AppendLine($"Disk: {stats.DiskUsedBytes} / {stats.DiskTotalBytes} byte");
        text.AppendLine($"Servrar: {stats.ServersRunning} av {stats.ServersTotal} körs");
        text.AppendLine();

        text.AppendLine("[Servrar]");
        if (status.Servers.Count == 0)
        {
            text.AppendLine("Inga servercontainrar.");
        }
        foreach (var server in status.Servers)
        {
            var cpu = server.CpuPercent is null ? "—" : $"{server.CpuPercent.Value:0.0} %";
            var memory = server.MemoryUsedMb is null ? "—" : $"{server.MemoryUsedMb} MiB";
            text.AppendLine(
                $"{server.ServerId} | {server.State} | cpu {cpu} | minne {memory} | {server.Image}");
        }
        text.AppendLine();

        if (!string.IsNullOrWhiteSpace(input.ComposePs))
        {
            text.AppendLine("[docker compose ps]");
            text.AppendLine(Redact(input.ComposePs).Trim());
            text.AppendLine();
        }

        text.AppendLine("[Agentlogg]");
        text.AppendLine(Redact(input.AgentLog).Trim());
        text.AppendLine();

        return text.ToString();
    }
}
