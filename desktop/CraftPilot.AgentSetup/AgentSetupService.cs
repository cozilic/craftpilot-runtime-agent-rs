using System.Collections.Concurrent;
using System.ComponentModel;
using System.Diagnostics;
using System.Net.Http;
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
    private readonly HostStatsCollector _hostStats = new();
    // Spelarantalet pingas mot serverns publicerade port. Statusfrågan går
    // var femte sekund, så svaren cachas kort i stället för att öppna en ny
    // TCP-anslutning varje gång.
    private readonly ConcurrentDictionary<int, (DateTime At, (int Online, int Max)? Value)> _players = new();
    // Senast lästa nodnamn. Loggen visar bara de 300 senaste raderna, så
    // namnet får inte försvinna ur panelen när agenten loggat vidare.
    private string? _nodeName;

    private readonly PanelSettingsStore _settings;

    public AgentSetupService(SetupSession session, PanelSettingsStore settings)
    {
        _session = session;
        _settings = settings;
    }

    /// <summary>
    /// Sant när användaren stoppade agenten i panelen. Uppstartsvakten startar
    /// då inte containern bakom ryggen; ett Starta nollställer markeringen.
    /// </summary>
    public bool AgentStoppedByUser => _settings.Load().AgentStoppedByUser;

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
            MasterUrl = MasterUrl.Default,
            Installed = File.Exists(ComposePath) && File.Exists(EnvPath),
            StartupEnabled = StartupRegistration.IsEnabled,
            Busy = _session.Busy,
        };

        var docker = await CheckDockerAsync(ct);

        status = CopyStatus(
            status,
            dockerOk: docker.State == DockerStateClassifier.Ready,
            dockerState: docker.State,
            dockerDesktopInstalled: docker.DesktopInstalled,
            dockerVersion: docker.Version,
            dockerError: docker.Error);

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

        if (docker.State == DockerStateClassifier.Ready)
        {
            var inspect = await RunAsync(
                "docker",
                ["inspect", "--format", "{{.State.Status}}\t{{.Config.Image}}", ContainerName],
                TimeSpan.FromSeconds(5),
                null,
                ct);

            status = CopyStatus(status, agentExists: inspect.ExitCode == 0);

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
                var logs = await GetComposeLogsAsync(ct);
                // En kod som skrivits ut efter en tidigare anslutning betyder att
                // noden tagits bort och agenten anmält sig igen. Då är det den
                // nya koden som gäller, inte den gamla "enrollment completed".
                var pendingCode = ClaimCodeParser.ExtractPending(logs);
                var completed = pendingCode is null
                    && logs.Contains(ClaimCodeParser.CompletedMarker, StringComparison.OrdinalIgnoreCase);
                _nodeName = NodeNameParser.Extract(logs) ?? _nodeName;
                status = CopyStatus(
                    status,
                    enrollmentCompleted: completed,
                    claimCode: pendingCode,
                    nodeName: _nodeName);
            }

            status = CopyStatus(status, sftp: await ReadSftpStatusAsync(ct));

            // Agentens servercontainrar bär labeln craftpilot.managed. Panelen
            // visar dem i översikten så det syns att noden faktiskt hostar
            // servrar, utan att fråga mastern.
            var servers = await RunAsync(
                "docker",
                ["ps", "-a", "--filter", "label=craftpilot.managed=1", "--format", ServerContainers.Format],
                TimeSpan.FromSeconds(5),
                null,
                ct);

            if (servers.ExitCode == 0)
            {
                var parsed = ServerContainers.Parse(servers.StdOut);
                status = CopyStatus(status, servers: await AttachStatsAsync(parsed, ct));
            }
        }

        // Serverkataloger utan container visas som "Inte startad", så en
        // nyskapad server syns redan innan den startats första gången.
        var merged = ServerContainers.WithServerDirectories(status.Servers, status.DataDir);
        var (playersOnline, playersMax, playersKnown) = await CountPlayersAsync(merged, ct);
        var stats = _hostStats.Collect(status.DataDir) with
        {
            ServersTotal = merged.Count,
            ServersRunning = merged.Count(server => server.Running),
            PlayersKnown = playersKnown,
            PlayersOnline = playersOnline,
            PlayersMax = playersMax,
        };

        return CopyStatus(status, servers: merged, stats: stats, nodeName: _nodeName);
    }

    /// <summary>
    /// Summerar spelarantalet från server-list-ping mot körda servrar. Servrar
    /// som inte svarar (fel port, annan speltyp eller fortfarande startar)
    /// räknas inte som noll spelare — då visas antalet som okänt i panelen.
    /// </summary>
    private async Task<(int Online, int? Max, bool Known)> CountPlayersAsync(
        IReadOnlyList<ServerContainer> servers,
        CancellationToken ct)
    {
        var targets = servers
            .Where(server => server.Running && server.HostPort is not null)
            .Select(server => server.HostPort!.Value)
            .Distinct()
            .ToList();

        if (targets.Count == 0)
        {
            return (0, null, false);
        }

        var now = DateTime.UtcNow;
        var results = await Task.WhenAll(targets.Select(async port =>
        {
            if (_players.TryGetValue(port, out var cached) && now - cached.At < TimeSpan.FromSeconds(5))
            {
                return cached.Value;
            }

            var value = await MinecraftStatusPing.QueryAsync(
                "127.0.0.1",
                port,
                TimeSpan.FromMilliseconds(800),
                ct);
            _players[port] = (now, value);
            return value;
        }));

        var answered = results
            .Where(result => result is not null)
            .Select(result => result!.Value)
            .ToList();

        return answered.Count == 0
            ? (0, null, false)
            : (answered.Sum(result => result.Online), answered.Sum(result => result.Max), true);
    }

    /// <summary>
    /// SFTPGo-containerns värdport och om tjänsten svarar. Porten läses ur
    /// Docker i stället för ur .env.desktop: agenten tar nästa lediga port när
    /// den föredragna är upptagen, så env-filen kan peka på en annan port än
    /// den som faktiskt används.
    /// </summary>
    private async Task<SftpStatus> ReadSftpStatusAsync(CancellationToken ct)
    {
        var enabled = SftpStatus.IsEnabled(ReadEnv("CRAFTPILOT_SFTP_ENABLED"));
        var preferred = SftpStatus.ParsePort(ReadEnv("CRAFTPILOT_SFTP_PORT"));

        var inspect = await RunAsync(
            "docker",
            ["inspect", "--format", "{{.State.Running}}\t{{json .HostConfig.PortBindings}}", SftpStatus.ContainerName],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        if (inspect.ExitCode != 0)
        {
            // Ingen container: agenten är inte installerad, eller så är SFTP avstängt.
            return new SftpStatus
            {
                Enabled = enabled,
                PreferredPort = preferred,
                Error = enabled ? FirstLine(inspect.Combined) : null,
            };
        }

        var parts = inspect.StdOut.Trim().Split('\t', 2);
        var running = string.Equals(parts.ElementAtOrDefault(0), "true", StringComparison.OrdinalIgnoreCase);
        var bound = SftpStatus.BoundPortFrom(parts.ElementAtOrDefault(1));
        var responding = running && bound is not null
            ? await SftpStatus.ProbeAsync(bound.Value, TimeSpan.FromMilliseconds(800), ct)
            : (bool?)null;

        return new SftpStatus
        {
            Enabled = enabled,
            PreferredPort = preferred,
            BoundPort = bound,
            Running = running,
            Responding = responding,
        };
    }

    public Task<OperationResponse> InstallAsync(InstallRequest request, CancellationToken ct = default) =>
        RunOperationAsync((output, token) => InstallCoreAsync(request, output, token), ct);

    public Task<OperationResponse> UpdateAsync(CancellationToken ct = default) =>
        RunOperationAsync(UpdateCoreAsync, ct);

    public Task<OperationResponse> StopAsync(CancellationToken ct = default) =>
        RunOperationAsync(StopCoreAsync, ct);

    /// Tar bort agenten från datorn: containern, installationsfilerna och
    /// kopplingen till noden. Serverdata och agentens state-volym lämnas kvar,
    /// så en ny installation kan återanvända credentialen.
    public Task<OperationResponse> RemoveAsync(CancellationToken ct = default) =>
        RunOperationAsync(RemoveCoreAsync, ct);

    /// Tar bort agentens servercontainrar som inte körs. Serverfilerna ligger
    /// kvar på disken, så nästa start skapar en ny container med samma data.
    public Task<OperationResponse> PruneServersAsync(CancellationToken ct = default) =>
        RunOperationAsync(PruneServersCoreAsync, ct);

    /// <summary>Startar om agentcontainern utan att röra serverdata.</summary>
    public Task<OperationResponse> RestartAgentAsync(CancellationToken ct = default) =>
        RunOperationAsync(RestartAgentCoreAsync, ct);

    /// <summary>Startar en agent som redan är installerad; hämtar ingen ny image.</summary>
    public Task<OperationResponse> StartAsync(CancellationToken ct = default) =>
        RunOperationAsync(StartAgentCoreAsync, ct);

    /// <summary>Startar om en servercontainer som panelen själv har hittat via labeln.</summary>
    public Task<OperationResponse> RestartServerAsync(string serverId, CancellationToken ct = default) =>
        RunOperationAsync((output, token) => RestartServerCoreAsync(serverId, output, token), ct);

    /// <summary>Slår SFTP på eller av på noden och startar om agenten med den nya flaggan.</summary>
    public Task<OperationResponse> SetSftpEnabledAsync(bool enabled, CancellationToken ct = default) =>
        RunOperationAsync((output, token) => SetSftpEnabledCoreAsync(enabled, output, token), ct);

    /// Kör en åtgärd med operation-lås, busy-flagga och Avbryt-stöd. Alla
    /// åtgärder går genom den här vägen, så panelen aldrig kan fastna i ett
    /// läge där en knapp är disabled utan motstycke.
    private async Task<OperationResponse> RunOperationAsync(
        Func<StringBuilder, CancellationToken, Task<OperationResponse>> action,
        CancellationToken ct)
    {
        var output = new StringBuilder();
        var operation = _session.BeginOperation(ct);

        try
        {
            await _session.OperationLock.WaitAsync(operation.Token);
        }
        catch (OperationCanceledException)
        {
            _session.EndOperation(operation);
            return Fail("Åtgärden avbröts.", output);
        }

        _session.Busy = true;
        try
        {
            return await action(output, operation.Token);
        }
        catch (OperationCanceledException)
        {
            return Fail("Åtgärden avbröts.", output);
        }
        finally
        {
            _session.Busy = false;
            _session.OperationLock.Release();
            _session.EndOperation(operation);
        }
    }

    /// <summary>
    /// Skriver CRAFTPILOT_SFTP_ENABLED i .env.desktop och återskapar
    /// agentcontainern, som läser flaggan vid start. Är flaggan av tar agenten
    /// bort SFTPGo-containern men lämnar volymerna, så värdnyckeln består.
    /// </summary>
    private async Task<OperationResponse> SetSftpEnabledCoreAsync(
        bool enabled,
        StringBuilder output,
        CancellationToken ct)
    {
        if (!File.Exists(EnvPath))
        {
            return Fail("Agenten är inte installerad ännu.", output);
        }

        try
        {
            var existing = await File.ReadAllTextAsync(EnvPath, ct);
            await File.WriteAllTextAsync(
                EnvPath,
                DesktopConfig.BuildEnvFile(ReadSavedDataDir(), enabled, existing),
                new UTF8Encoding(false),
                ct);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return Fail($"Kunde inte skriva {EnvFileName}: {ex.Message}", output);
        }

        output.AppendLine(enabled
            ? $"{DesktopConfig.SftpEnabledKey}=true i {EnvFileName}."
            : $"{DesktopConfig.SftpEnabledKey}=false i {EnvFileName}.");

        if (!File.Exists(ComposePath))
        {
            // Ingen compose-fil: nästa installation använder den sparade flaggan.
            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        // Agenten läser flaggan vid start, så en körande agent måste återskapas
        // med den nya miljön. Är agenten stoppad sparas bara flaggan: den ska
        // inte startas som en bieffekt av en SFTP-ändring (panelen låser också
        // SFTP-knapparna då).
        if (!await IsAgentRunningAsync(ct))
        {
            output.AppendLine("Agenten är stoppad — inställningen gäller nästa gång den startar.");
            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }

        // State-volymen och serverdatan behålls.
        var up = await ComposeAsync(
            ["up", "-d", "--force-recreate", "agent"],
            TimeSpan.FromMinutes(5),
            ct);
        Append(output, up);
        if (up.ExitCode != 0)
        {
            return Fail("Kunde inte starta om agenten med den nya inställningen.", output);
        }

        await Task.Delay(TimeSpan.FromSeconds(2.5), ct);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<OperationResponse> InstallCoreAsync(
        InstallRequest request,
        StringBuilder output,
        CancellationToken ct)
    {
        if (!MasterUrl.TryNormalize(request.MasterUrl ?? MasterUrl.Default, out var master, out var error))
        {
            return Fail(error, output);
        }

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            if (!string.IsNullOrWhiteSpace(docker.Error))
            {
                output.AppendLine(docker.Error);
            }

            return Fail(DockerGuidance(docker.State), output);
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
        var existingEnv = File.Exists(EnvPath) ? await File.ReadAllTextAsync(EnvPath, ct) : null;
        await File.WriteAllTextAsync(
            EnvPath,
            DesktopConfig.BuildEnvFile(
                dataDirectory,
                DesktopConfig.ReadSftpEnabled(existingEnv),
                existingEnv),
            new UTF8Encoding(false),
            ct);

        output.AppendLine($"Master: {master}");
        output.AppendLine($"Serverdata: {dataDirectory}");
        output.AppendLine();

        if (!await EnsureNetworkAsync(output, ct))
        {
            return Fail($"Kunde inte skapa Docker-nätverket {NetworkName}.", output);
        }

        var pull = await ComposeAsync(["pull"], TimeSpan.FromMinutes(5), ct);
        Append(output, pull);
        if (pull.ExitCode != 0)
        {
            return Fail("Kunde inte hämta agentimagen.", output);
        }

        if (await StartAgentAsync(output, ct) != 0)
        {
            return Fail("Kunde inte starta agenten.", output);
        }

        _settings.SaveAgentStoppedByUser(false);
        await Task.Delay(TimeSpan.FromSeconds(2.5), ct);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    /// <summary>Sant när agentcontainern finns och kör, inte bara existerar.</summary>
    private async Task<bool> IsAgentRunningAsync(CancellationToken ct)
    {
        var inspect = await RunAsync(
            "docker",
            ["inspect", "--format", "{{.State.Status}}", ContainerName],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        return inspect.ExitCode == 0 &&
            string.Equals(inspect.StdOut.Trim(), "running", StringComparison.OrdinalIgnoreCase);
    }

    private async Task<OperationResponse> StartAgentCoreAsync(
        StringBuilder output,
        CancellationToken ct)
    {
        if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
        {
            return Fail("Agenten är inte installerad ännu.", output);
        }

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        // Samma väg som installationen: en kvarvarande container med samma
        // namn tas bort först, sedan `compose up -d` med befintlig .env.
        if (await StartAgentAsync(output, ct) != 0)
        {
            return Fail("Kunde inte starta agenten.", output);
        }

        _settings.SaveAgentStoppedByUser(false);
        await Task.Delay(TimeSpan.FromSeconds(2.5), ct);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<OperationResponse> UpdateCoreAsync(StringBuilder output, CancellationToken ct)
    {
        if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
        {
            return Fail("Agenten är inte installerad ännu.", output);
        }

        var existingEnv = await File.ReadAllTextAsync(EnvPath, ct);
        await File.WriteAllTextAsync(
            EnvPath,
            DesktopConfig.BuildEnvFile(
                ReadSavedDataDir(),
                DesktopConfig.ReadSftpEnabled(existingEnv),
                existingEnv),
            new UTF8Encoding(false),
            ct);

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        var pull = await ComposeAsync(["pull"], TimeSpan.FromMinutes(5), ct);
        Append(output, pull);
        if (pull.ExitCode != 0)
        {
            return Fail("Kunde inte hämta den senaste agentimagen.", output);
        }

        if (await StartAgentAsync(output, ct) != 0)
        {
            return Fail("Kunde inte starta om agenten.", output);
        }

        _settings.SaveAgentStoppedByUser(false);
        await Task.Delay(TimeSpan.FromSeconds(1.5), ct);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<OperationResponse> PruneServersCoreAsync(StringBuilder output, CancellationToken ct)
    {
        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        var list = await RunAsync(
            "docker",
            ["ps", "-a", "--filter", "label=craftpilot.managed=1", "--format", ServerContainers.Format],
            TimeSpan.FromSeconds(10),
            null,
            ct);
        if (list.ExitCode != 0)
        {
            return Fail("Kunde inte läsa serverlistan från Docker.", output);
        }

        // Bara stoppade containrar med agentens eget namn rörs. Körda servrar
        // och containrar som något annat skapat lämnas i fred.
        var stopped = ServerContainers.Stopped(ServerContainers.Parse(list.StdOut));
        if (stopped.Count == 0)
        {
            output.AppendLine("Inga stoppade servercontainrar att rensa.");
            return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
        }

        var failed = 0;
        foreach (var server in stopped)
        {
            var remove = await RunAsync("docker", ["rm", server.Name], TimeSpan.FromSeconds(30), null, ct);
            if (remove.ExitCode == 0)
            {
                output.AppendLine($"Borttagen: {server.Name}");
            }
            else
            {
                failed++;
                output.AppendLine($"Kunde inte ta bort {server.Name}: {FirstLine(remove.Combined)}");
            }
        }

        if (failed > 0)
        {
            return Fail($"{failed} av {stopped.Count} stoppade containrar kunde inte tas bort.", output);
        }

        output.AppendLine($"Klart: {stopped.Count} stoppade containrar borttagna. Serverfilerna ligger kvar på disken.");
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    /// <summary>
    /// Ett enda docker stats-anrop för alla körande servrar. Ett fel ger
    /// oförändrade rader: statistiken är ett komplement, inte ett krav.
    /// </summary>
    private async Task<IReadOnlyList<ServerContainer>> AttachStatsAsync(
        IReadOnlyList<ServerContainer> servers,
        CancellationToken ct)
    {
        var running = servers
            .Where(server => server.Running && ServerContainers.IsManagedName(server.Name))
            .ToList();
        if (running.Count == 0)
        {
            return servers;
        }

        var arguments = new List<string> { "stats", "--no-stream", "--format", ServerStats.Format };
        arguments.AddRange(running.Select(server => server.Name));

        var result = await RunAsync("docker", arguments, TimeSpan.FromSeconds(10), null, ct);
        if (result.ExitCode != 0)
        {
            return servers;
        }

        var usage = ServerStats.Parse(result.StdOut);
        return servers
            .Select(server => usage.TryGetValue(server.Name, out var value)
                ? server with
                {
                    CpuPercent = value.CpuPercent,
                    MemoryUsedMb = value.MemoryUsedMb,
                    MemoryLimitMb = value.MemoryLimitMb,
                }
                : server)
            .ToList();
    }

    private async Task<OperationResponse> RestartAgentCoreAsync(StringBuilder output, CancellationToken ct)
    {
        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
        {
            var existing = await RunAsync(
                "docker",
                ["inspect", "--format", "{{.Id}}", ContainerName],
                TimeSpan.FromSeconds(5),
                null,
                ct);
            if (existing.ExitCode != 0)
            {
                return Fail("Agenten är inte installerad.", output);
            }

            var restart = await RunAsync(
                "docker",
                ["restart", ContainerName],
                TimeSpan.FromMinutes(2),
                null,
                ct);
            Append(output, restart);
            return restart.ExitCode == 0
                ? new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct))
                : Fail("Kunde inte starta om agenten.", output);
        }

        var compose = await ComposeAsync(["restart", "agent"], TimeSpan.FromMinutes(2), ct);
        Append(output, compose);
        if (compose.ExitCode != 0)
        {
            return Fail("Kunde inte starta om agenten.", output);
        }

        await Task.Delay(TimeSpan.FromSeconds(1.5), ct);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    /// <summary>
    /// Startar bara om en container vars namn och server-id båda matchar
    /// anropet. Ingen container adresseras med ett namn från HTTP-input.
    /// </summary>
    private async Task<OperationResponse> RestartServerCoreAsync(
        string serverId,
        StringBuilder output,
        CancellationToken ct)
    {
        if (!Guid.TryParse(serverId, out var id))
        {
            return Fail("Server-id:t är ogiltigt.", output);
        }

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        var list = await RunAsync(
            "docker",
            ["ps", "-a", "--filter", "label=craftpilot.managed=1", "--format", ServerContainers.Format],
            TimeSpan.FromSeconds(10),
            null,
            ct);
        if (list.ExitCode != 0)
        {
            return Fail("Kunde inte läsa serverlistan från Docker.", output);
        }

        var expected = ServerContainers.ExpectedName(id.ToString());
        var server = ServerContainers.Parse(list.StdOut).FirstOrDefault(candidate =>
            string.Equals(candidate.Name, expected, StringComparison.Ordinal) &&
            string.Equals(candidate.ServerId, id.ToString(), StringComparison.OrdinalIgnoreCase));
        if (server is null)
        {
            return Fail("Servern finns inte på den här noden.", output);
        }

        var restart = await RunAsync(
            "docker",
            ["restart", "-t", "30", server.Name],
            TimeSpan.FromMinutes(2),
            null,
            ct);
        Append(output, restart);
        if (restart.ExitCode != 0)
        {
            return Fail("Kunde inte starta om servern.", output);
        }

        output.AppendLine($"Startade om {server.Name}.");
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<OperationResponse> StopCoreAsync(StringBuilder output, CancellationToken ct)
    {
        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        if (File.Exists(ComposePath) && File.Exists(EnvPath))
        {
            var down = await ComposeAsync(["down", "--remove-orphans"], TimeSpan.FromMinutes(2), ct);
            Append(output, down);
            if (down.ExitCode != 0)
            {
                return Fail("Kunde inte stoppa agenten.", output);
            }
        }

        // En container som skapats av ett annat projekt stoppas inte av
        // `compose down`, så den tas bort här också.
        if (!await RemoveContainerAsync(output, ct))
        {
            return Fail("Kunde inte ta bort agentcontainern.", output);
        }

        _settings.SaveAgentStoppedByUser(true);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<OperationResponse> RemoveCoreAsync(StringBuilder output, CancellationToken ct)
    {
        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return Fail(DockerGuidance(docker.State), output);
        }

        if (File.Exists(ComposePath) && File.Exists(EnvPath))
        {
            var down = await ComposeAsync(["down", "--remove-orphans"], TimeSpan.FromMinutes(2), ct);
            Append(output, down);
        }

        if (!await RemoveContainerAsync(output, ct))
        {
            return Fail("Kunde inte ta bort agentcontainern.", output);
        }

        foreach (var file in new[] { ComposePath, EnvPath })
        {
            try
            {
                if (File.Exists(file))
                {
                    File.Delete(file);
                }
            }
            catch (Exception ex)
            {
                return Fail($"Kunde inte ta bort {file}: {ex.Message}", output);
            }
        }

        output.AppendLine("Agenten är borttagen från datorn. Serverdata ligger kvar, liksom agentens state-volym.");
        _settings.SaveAgentStoppedByUser(false);
        return new OperationResponse(true, output.ToString().Trim(), null, await GetStatusAsync(ct));
    }

    private async Task<bool> EnsureNetworkAsync(StringBuilder output, CancellationToken ct)
    {
        var network = await RunAsync(
            "docker",
            ["network", "inspect", NetworkName],
            TimeSpan.FromSeconds(10),
            null,
            ct);

        if (network.ExitCode == 0)
        {
            output.AppendLine($"Docker-nätverket {NetworkName} finns redan.");
            return true;
        }

        var create = await RunAsync(
            "docker",
            ["network", "create", NetworkName],
            TimeSpan.FromSeconds(20),
            null,
            ct);

        Append(output, create);
        return create.ExitCode == 0;
    }

    /// Startar agenten, men tar först bort en kvarvarande container med samma
    /// namn.
    ///
    /// Compose återanvänder aldrig ett container-namn som skapats av ett annat
    /// projekt (en tidigare installation, `runtime-agent/start-rs-agent.ps1`
    /// eller en annan compose-fil): då faller `up` med "is already in use by
    /// container". Namnet ägs av panelen, så en gammal container tas bort
    /// först. Utan det fastnar installationen helt — det finns ingen knapp som
    /// tar bort den.
    private async Task<int> StartAgentAsync(StringBuilder output, CancellationToken ct)
    {
        if (!await RemoveContainerAsync(output, ct))
        {
            return -1;
        }

        var up = await ComposeAsync(["up", "-d"], TimeSpan.FromMinutes(2), ct);
        Append(output, up);
        if (up.ExitCode != 0 && ComposeConflict.IsNameConflict(up.Combined))
        {
            output.AppendLine("Containernamnet krockade — tar bort den gamla containern och försöker igen.");
            if (!await RemoveContainerAsync(output, ct))
            {
                return -1;
            }

            up = await ComposeAsync(["up", "-d"], TimeSpan.FromMinutes(2), ct);
            Append(output, up);
        }

        return up.ExitCode;
    }

    private async Task<bool> RemoveContainerAsync(StringBuilder output, CancellationToken ct)
    {
        var inspect = await RunAsync(
            "docker",
            ["inspect", "--format", "{{.Id}}", ContainerName],
            TimeSpan.FromSeconds(10),
            null,
            ct);

        if (inspect.ExitCode != 0)
        {
            return true;
        }

        output.AppendLine($"Tar bort en kvarvarande container med namnet {ContainerName}.");
        var remove = await RunAsync(
            "docker",
            ["rm", "-f", ContainerName],
            TimeSpan.FromMinutes(1),
            null,
            ct);

        Append(output, remove);
        return remove.ExitCode == 0;
    }

    public async Task<string> GetLogsAsync(CancellationToken ct = default)
    {
        if (!File.Exists(ComposePath) || !File.Exists(EnvPath))
        {
            return string.Empty;
        }

        var docker = await CheckDockerAsync(ct);
        if (docker.State != DockerStateClassifier.Ready)
        {
            return DockerGuidance(docker.State);
        }

        return await GetComposeLogsAsync(ct);
    }

    /// <summary>
    /// Samlar panelens, Dockers och agentens läge i en textfil för support.
    /// Innehållet redigeras av DiagnosticsReport innan det lämnar panelen.
    /// </summary>
    public async Task<string> GetDiagnosticsAsync(CancellationToken ct = default)
    {
        var status = await GetStatusAsync(ct);
        var log = await GetLogsAsync(ct);
        var composeVersion = await RunAsync(
            "docker",
            ["compose", "version", "--short"],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        ProcessResult? ps = null;
        if (File.Exists(ComposePath) && File.Exists(EnvPath))
        {
            ps = await ComposeAsync(["ps"], TimeSpan.FromSeconds(15), ct);
        }

        return DiagnosticsReport.Build(new DiagnosticsInput(
            PanelVersion.Current,
            DateTimeOffset.Now,
            Environment.MachineName,
            System.Runtime.InteropServices.RuntimeInformation.OSDescription,
            status,
            log,
            ps?.Combined,
            composeVersion.ExitCode == 0 ? composeVersion.StdOut.Trim() : null));
    }

    private async Task<string> GetComposeLogsAsync(CancellationToken ct)
    {
        var logs = await ComposeAsync(
            ["logs", "--no-color", "--tail", "300", "agent"],
            TimeSpan.FromSeconds(20),
            ct);

        return ClaimCodeParser.StripAnsi(logs.Combined).Trim();
    }

    private async Task<DockerCheck> CheckDockerAsync(CancellationToken ct)
    {
        var result = await RunAsync(
            "docker",
            ["version", "--format", "{{.Server.Version}}"],
            TimeSpan.FromSeconds(5),
            null,
            ct);

        var state = DockerStateClassifier.Classify(result.ExitCode, result.Combined, result.StartFailed);
        return new DockerCheck(
            state,
            result.ExitCode == 0 ? result.StdOut.Trim() : null,
            result.ExitCode == 0 ? null : FirstLine(result.Combined),
            IsDockerDesktopInstalled());
    }

    private static string DockerGuidance(string state) => state switch
    {
        DockerStateClassifier.NotInstalled =>
            "Docker Desktop är inte installerat. Ladda ner Docker Desktop med Linux-containrar, starta det och försök igen.",
        DockerStateClassifier.NotRunning =>
            "Docker Desktop är installerat men motorn är inte igång. Starta Docker Desktop och vänta tills Docker-motorn är redo.",
        _ => "Docker svarar inte. Kontrollera Docker Desktop och försök igen.",
    };

    private static bool IsDockerDesktopInstalled()
    {
        if (!OperatingSystem.IsWindows())
        {
            return false;
        }

        var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
        var localAppData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);

        return new[]
        {
            Path.Combine(programFiles, "Docker", "Docker", "Docker Desktop.exe"),
            Path.Combine(localAppData, "Docker", "Docker Desktop.exe"),
            Path.Combine(localAppData, "Programs", "Docker", "Docker Desktop.exe"),
        }.Any(File.Exists);
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
            return new ProcessResult(-1, string.Empty, ex.Message, StartFailed: true);
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
        string? dockerState = null,
        bool? dockerDesktopInstalled = null,
        string? dockerVersion = null,
        string? dockerError = null,
        bool? masterOk = null,
        int? masterHttpStatus = null,
        string? masterVersion = null,
        long? masterLatencyMs = null,
        string? masterError = null,
        bool? agentRunning = null,
        bool? agentExists = null,
        string? agentStatus = null,
        string? agentImage = null,
        string? agentError = null,
        bool? enrollmentCompleted = null,
        string? claimCode = null,
        string? nodeName = null,
        HostStats? stats = null,
        IReadOnlyList<ServerContainer>? servers = null,
        SftpStatus? sftp = null) =>
        new()
        {
            DockerOk = dockerOk ?? source.DockerOk,
            DockerState = dockerState ?? source.DockerState,
            DockerDesktopInstalled = dockerDesktopInstalled ?? source.DockerDesktopInstalled,
            DockerVersion = dockerVersion ?? source.DockerVersion,
            DockerError = dockerError ?? source.DockerError,
            MasterOk = masterOk ?? source.MasterOk,
            MasterHttpStatus = masterHttpStatus ?? source.MasterHttpStatus,
            MasterVersion = masterVersion ?? source.MasterVersion,
            MasterLatencyMs = masterLatencyMs ?? source.MasterLatencyMs,
            MasterError = masterError ?? source.MasterError,
            MasterUrl = source.MasterUrl,
            AgentRunning = agentRunning ?? source.AgentRunning,
            AgentExists = agentExists ?? source.AgentExists,
            AgentStatus = agentStatus ?? source.AgentStatus,
            AgentImage = agentImage ?? source.AgentImage,
            AgentError = agentError ?? source.AgentError,
            Installed = source.Installed,
            EnrollmentCompleted = enrollmentCompleted ?? source.EnrollmentCompleted,
            ClaimCode = claimCode ?? source.ClaimCode,
            NodeName = nodeName ?? source.NodeName,
            HostName = source.HostName,
            DataDir = source.DataDir,
            InstallDir = source.InstallDir,
            StartupEnabled = source.StartupEnabled,
            Busy = source.Busy,
            Stats = stats ?? source.Stats,
            Servers = servers ?? source.Servers,
            Sftp = sftp ?? source.Sftp,
        };

    private sealed record DockerCheck(string State, string? Version, string? Error, bool DesktopInstalled);

    private sealed record ProcessResult(int ExitCode, string StdOut, string StdErr, bool StartFailed = false)
    {
        public string Combined => string.Join(
            Environment.NewLine,
            new[] { StdOut.Trim(), StdErr.Trim() }.Where(value => value.Length > 0));
    }
}
