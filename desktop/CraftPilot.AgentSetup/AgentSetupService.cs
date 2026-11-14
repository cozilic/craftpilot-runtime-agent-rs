using System.ComponentModel;
using System.Diagnostics;
using System.Reflection;
using System.Text;
using System.Text.Json;

namespace CraftPilot.AgentSetup;

public sealed class AgentSetupService
{
    private const string ContainerName = "craftpilot-runtime-agent-rs";
    private const string ComposeFileName = "compose.desktop.yaml";
    private const string EnvFileName = ".env.desktop";
    private const string NetworkName = "craftpilot-servers";
    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(5) };
    private readonly SetupSession _session;

    public AgentSetupService(SetupSession session) => _session = session;

    public string InstallDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "CraftPilot",
        "Agent");

    public string DefaultDataDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
        "CraftPilot",
        "servers");

    private string ComposePath => Path.Combine(InstallDirectory, ComposeFileName);
    private string EnvPath => Path.Combine(InstallDirectory, EnvFileName);

    public async Task<StatusResponse> GetStatusAsync(CancellationToken ct = default)
    {
        var status = new StatusResponse
        {
            InstallDir = InstallDirectory,
            DataDir = ReadSavedDataDir(),
            MasterUrl = ReadSavedMasterUrl(),
            Installed = File.Exists(ComposePath) && File.Exists(EnvPath),
            Busy = _session.Busy,
        };

        var docker = await RunAsync(
            "docker",
            ["version", "--format", "{{.Server.Version}}"],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        status = CopyStatus(
            status,
            dockerOk: docker.ExitCode == 0,
            dockerVersion: docker.ExitCode == 0 ? docker.StdOut.Trim() : null,
            dockerError: docker.ExitCode == 0 ? null : FirstLine(docker.Combined));

        if (MasterUrl.TryNormalize(status.MasterUrl, out var master, out var masterError))
        {
            var stopwatch = Stopwatch.StartNew();
            try
            {
                using var response = await Http.GetAsync($"{master}/api/health", ct);
                status = CopyStatus(status, masterHttpStatus: (int)response.StatusCode);
                var body = await response.Content.ReadAsStringAsync(ct);
                var healthy = false;
                string? version = null;

                try
                {
                    using var document = JsonDocument.Parse(body);
                    healthy = document.RootElement.TryGetProperty("status", out var statusValue) &&
                        string.Equals(statusValue.GetString(), "ok", StringComparison.OrdinalIgnoreCase);
                    if (document.RootElement.TryGetProperty("version", out var versionValue))
                    {
                        version = versionValue.GetString();
                    }
                }
                catch (JsonException)
                {
                    // A reachable master with an unexpected body is still shown as red.
                }

                status = CopyStatus(
                    status,
                    masterOk: response.IsSuccessStatusCode && healthy,
                    masterVersion: version,
                    masterError: response.IsSuccessStatusCode && healthy ? null : $"HTTP {(int)response.StatusCode}");
            }
            catch (TaskCanceledException) when (!ct.IsCancellationRequested)
            {
                status = CopyStatus(status, masterError: "Tidsgränsen överskreds.");
            }
            catch (Exception ex)
            {
                status = CopyStatus(status, masterError: FirstLine(ex.Message));
            }

            status = CopyStatus(status, masterLatencyMs: stopwatch.ElapsedMilliseconds);
        }
        else
        {
            status = CopyStatus(status, masterError: masterError);
        }

        var inspect = await RunAsync(
            "docker",
            ["inspect", "--format", "{{.State.Status}}\t{{.Config.Image}}", ContainerName],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        if (inspect.ExitCode == 0)
        {
            var parts = inspect.StdOut.Trim().Split('\t', 2);
            status = CopyStatus(
                status,
                agentStatus: parts.ElementAtOrDefault(0),
                agentImage: parts.ElementAtOrDefault(1),
                agentRunning: string.Equals(parts.ElementAtOrDefault(0), "running", StringComparison.OrdinalIgnoreCase));
        }
        else
        {
            status = CopyStatus(status, agentError: FirstLine(inspect.Combined));
        }

        if (status.Installed)
        {
            var logs = await GetLogsAsync(ct);
            var completed = logs.Contains("enrollment completed", StringComparison.OrdinalIgnoreCase);
            status = CopyStatus(
                status,
                enrollmentCompleted: completed,
                claimCode: completed ? null : ClaimCodeParser.Extract(logs));
        }

        return status;
    }

    public async Task<OperationResponse> InstallAsync(InstallRequest request, CancellationToken ct = default)
    {
        await _session.OperationLock.WaitAsync(ct);
        _session.Busy = true;
        var output = new StringBuilder();

        try
        {
            if (!MasterUrl.TryNormalize(request.MasterUrl ?? MasterUrl.Default, out var master, out var error))
            {
                return Fail(error, output);
            }

            string dataDirectory;
            try
            {
                dataDirectory = Path.GetFullPath(
                    string.IsNullOrWhiteSpace(request.DataDir) ? DefaultDataDirectory : request.DataDir.Trim());
                Directory.CreateDirectory(InstallDirectory);
                Directory.CreateDirectory(dataDirectory);
            }
            catch (Exception ex)
            {
                return Fail($"Kunde inte skapa katalogerna: {ex.Message}", output);
            }

            WriteCompose();
            await File.WriteAllTextAsync(
                EnvPath,
                DesktopConfig.BuildEnvFile(master, dataDirectory),
                new UTF8Encoding(false),
                ct);

            output.AppendLine($"Master: {master}");
            output.AppendLine($"Serverdata: {dataDirectory}");
            output.AppendLine();

            var network = await RunAsync(
                "docker",
                ["network", "inspect", NetworkName],
                TimeSpan.FromSeconds(10),
                null,
                ct);

            if (network.ExitCode != 0)
            {
                var create = await RunAsync(
                    "docker",
                    ["network", "create", NetworkName],
                    TimeSpan.FromSeconds(20),
                    null,
                    ct);
                Append(output, create);
                if (create.ExitCode != 0)
                {
                    return Fail($"Kunde inte skapa Docker-nätverket {NetworkName}.", output);
                }
            }
            else
            {
                output.AppendLine($"Docker-nätverket {NetworkName} finns redan.");
            }

            var pull = await ComposeAsync(["pull"], TimeSpan.FromMinutes(5), ct);
            Append(output, pull);
            if (pull.ExitCode != 0)
            {
                return Fail("Kunde inte hämta agentimagen.", output);
            }

            var up = await ComposeAsync(["up", "-d"], TimeSpan.FromMinutes(2), ct);
            Append(output, up);
            if (up.ExitCode != 0)
            {
                return Fail("Kunde inte starta agenten.", output);
            }

            await Task.Delay(TimeSpan.FromSeconds(2.5), ct);
            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }
        catch (OperationCanceledException)
        {
            return Fail("Åtgärden avbröts.", output);
        }
        finally
        {
            _session.Busy = false;
            _session.OperationLock.Release();
        }
    }

    public async Task<OperationResponse> UpdateAsync(CancellationToken ct = default)
    {
        await _session.OperationLock.WaitAsync(ct);
        _session.Busy = true;
        var output = new StringBuilder();

        try
        {
            if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
            {
                return Fail("Agenten är inte installerad ännu.", output);
            }

            var pull = await ComposeAsync(["pull"], TimeSpan.FromMinutes(5), ct);
            Append(output, pull);
            if (pull.ExitCode != 0)
            {
                return Fail("Kunde inte hämta den senaste agentimagen.", output);
            }

            var up = await ComposeAsync(["up", "-d"], TimeSpan.FromMinutes(2), ct);
            Append(output, up);
            if (up.ExitCode != 0)
            {
                return Fail("Kunde inte starta om agenten.", output);
            }

            await Task.Delay(TimeSpan.FromSeconds(1.5), ct);
            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }
        catch (OperationCanceledException)
        {
            return Fail("Åtgärden avbröts.", output);
        }
        finally
        {
            _session.Busy = false;
            _session.OperationLock.Release();
        }
    }

    public async Task<OperationResponse> StopAsync(CancellationToken ct = default)
    {
        await _session.OperationLock.WaitAsync(ct);
        _session.Busy = true;
        var output = new StringBuilder();

        try
        {
            if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
            {
                return Fail("Agenten är inte installerad ännu.", output);
            }

            var down = await ComposeAsync(["down"], TimeSpan.FromMinutes(2), ct);
            Append(output, down);
            if (down.ExitCode != 0)
            {
                return Fail("Kunde inte stoppa agenten.", output);
            }

            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }
        catch (OperationCanceledException)
        {
            return Fail("Åtgärden avbröts.", output);
        }
        finally
        {
            _session.Busy = false;
            _session.OperationLock.Release();
        }
    }

    public async Task<string> GetLogsAsync(CancellationToken ct = default)
    {
        if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
        {
            return string.Empty;
        }

        var logs = await ComposeAsync(
            ["logs", "--no-color", "--tail", "300", "agent"],
            TimeSpan.FromSeconds(20),
            ct);

        return ClaimCodeParser.StripAnsi(logs.Combined).Trim();
    }

    public void OpenInstallFolder()
    {
        Directory.CreateDirectory(InstallDirectory);
        Process.Start(new ProcessStartInfo
        {
            FileName = InstallDirectory,
            UseShellExecute = true,
        });
    }

    private async Task<ProcessResult> ComposeAsync(
        IReadOnlyList<string> arguments,
        TimeSpan timeout,
        CancellationToken ct)
    {
        var allArguments = new List<string>
        {
            "compose",
            "--env-file",
            EnvPath,
            "-f",
            ComposePath,
        };
        allArguments.AddRange(arguments);

        return await RunAsync("docker", allArguments, timeout, InstallDirectory, ct);
    }

    private void WriteCompose()
    {
        using var resource = Assembly.GetExecutingAssembly()
            .GetManifestResourceStream("compose.desktop.yaml")
            ?? throw new InvalidOperationException("compose.desktop.yaml saknas i appen.");

        using var reader = new StreamReader(resource);
        File.WriteAllText(ComposePath, reader.ReadToEnd(), new UTF8Encoding(false));
    }

    private string ReadSavedMasterUrl() => ReadEnv("CRAFTPILOT_MASTER_URL") ?? MasterUrl.Default;

    private string ReadSavedDataDir() =>
        ReadEnv("CRAFTPILOT_HOST_DATA_DIR") ?? DefaultDataDirectory;

    private string? ReadEnv(string key)
    {
        if (!File.Exists(EnvPath))
        {
            return null;
        }

        foreach (var line in File.ReadLines(EnvPath))
        {
            var separator = line.IndexOf('=');
            if (separator > 0 &&
                string.Equals(line[..separator], key, StringComparison.Ordinal))
            {
                return line[(separator + 1)..];
            }
        }

        return null;
    }

    private static async Task<ProcessResult> RunAsync(
        string fileName,
        IReadOnlyList<string> arguments,
        TimeSpan timeout,
        string? workingDirectory,
        CancellationToken ct)
    {
        var startInfo = new ProcessStartInfo(fileName)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            WorkingDirectory = workingDirectory ?? Environment.CurrentDirectory,
        };

        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        using var process = new Process { StartInfo = startInfo };
        try
        {
            process.Start();
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException)
        {
            return new ProcessResult(-1, string.Empty, ex.Message);
        }

        var stdoutTask = process.StandardOutput.ReadToEndAsync();
        var stderrTask = process.StandardError.ReadToEndAsync();
        using var timeoutSource = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeoutSource.CancelAfter(timeout);

        try
        {
            await process.WaitForExitAsync(timeoutSource.Token);
        }
        catch (OperationCanceledException)
        {
            try
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                }
            }
            catch
            {
                // The process may already have exited.
            }

            var partialOut = await SafeReadAsync(stdoutTask);
            var partialError = await SafeReadAsync(stderrTask);
            return new ProcessResult(
                -1,
                partialOut,
                string.Join(Environment.NewLine, partialError, "Tidsgränsen överskreds.").Trim());
        }

        return new ProcessResult(
            process.ExitCode,
            await stdoutTask,
            await stderrTask);
    }

    private static async Task<string> SafeReadAsync(Task<string> read)
    {
        try
        {
            return await read;
        }
        catch
        {
            return string.Empty;
        }
    }

    private static void Append(StringBuilder output, ProcessResult result)
    {
        var text = result.Combined;
        if (!string.IsNullOrWhiteSpace(text))
        {
            output.AppendLine(text);
        }
    }

    private static OperationResponse Fail(string error, StringBuilder output) =>
        new(false, output.ToString().Trim(), error, null);

    private static string FirstLine(string value) =>
        ClaimCodeParser.StripAnsi(value)
            .Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => line.Trim())
            .FirstOrDefault(line => line.Length > 0)
        ?? "Okänt fel";

    private static StatusResponse CopyStatus(
        StatusResponse source,
        bool? dockerOk = null,
        string? dockerVersion = null,
        string? dockerError = null,
        bool? masterOk = null,
        int? masterHttpStatus = null,
        string? masterVersion = null,
        long? masterLatencyMs = null,
        string? masterError = null,
        bool? agentRunning = null,
        string? agentStatus = null,
        string? agentImage = null,
        string? agentError = null,
        bool? enrollmentCompleted = null,
        string? claimCode = null) =>
        new()
        {
            DockerOk = dockerOk ?? source.DockerOk,
            DockerVersion = dockerVersion ?? source.DockerVersion,
            DockerError = dockerError ?? source.DockerError,
            MasterOk = masterOk ?? source.MasterOk,
            MasterHttpStatus = masterHttpStatus ?? source.MasterHttpStatus,
            MasterVersion = masterVersion ?? source.MasterVersion,
            MasterLatencyMs = masterLatencyMs ?? source.MasterLatencyMs,
            MasterError = masterError ?? source.MasterError,
            MasterUrl = source.MasterUrl,
            AgentRunning = agentRunning ?? source.AgentRunning,
            AgentStatus = agentStatus ?? source.AgentStatus,
            AgentImage = agentImage ?? source.AgentImage,
            AgentError = agentError ?? source.AgentError,
            Installed = source.Installed,
            EnrollmentCompleted = enrollmentCompleted ?? source.EnrollmentCompleted,
            ClaimCode = claimCode ?? source.ClaimCode,
            DataDir = source.DataDir,
            InstallDir = source.InstallDir,
            Busy = source.Busy,
        };

    private sealed record ProcessResult(int ExitCode, string StdOut, string StdErr)
    {
        public string Combined => string.Join(
            Environment.NewLine,
            new[] { StdOut.Trim(), StdErr.Trim() }.Where(value => value.Length > 0));
    }
}
