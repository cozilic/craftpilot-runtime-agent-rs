using System.Diagnostics;
using System.Net;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;

using CraftPilot.AgentSetup;

var builder = WebApplication.CreateBuilder(args);
var startInTray = StartupRegistration.IsStartupInvocation(args);

// The dashboard controls a privileged Docker socket. Keep it on loopback and
// require a per-run token on every API call; the page gets the token inline.
builder.WebHost.ConfigureKestrel(options => options.Listen(IPAddress.Loopback, 0));
builder.Services.AddSingleton<SetupSession>();
builder.Services.AddSingleton<AgentSetupService>();
builder.Services.AddSingleton<PanelSettingsStore>();
builder.Services.AddSingleton<UpdateChecker>();
builder.Services.AddSingleton<PanelUpdater>();
builder.Services.AddSingleton<DockerDesktopInstaller>();
builder.Logging.ClearProviders();

var app = builder.Build();

// En självuppdatering lämnar förra panelversionen som .old bredvid exen.
try
{
    if (Environment.ProcessPath is { } processPath && File.Exists(processPath + ".old"))
    {
        File.Delete(processPath + ".old");
    }
}
catch
{
    // Filen kan vara låst av en pågående uppdatering; den städas nästa gång.
}

var session = app.Services.GetRequiredService<SetupSession>();
var assembly = typeof(SetupSession).Assembly;

var indexHtml = ReadResource("wwwroot.index.html")
    .Replace("__CRAFT_PILOT_TOKEN__", session.Token, StringComparison.Ordinal);

app.Use(async (context, next) =>
{
    // Loopback räcker inte: en webbsida på en annan origin kan posta hit, och
    // DNS-rebinding kan få en angripares domän att peka på 127.0.0.1. Vakten
    // kräver rätt Host, sessionsnyckeln på /api och att tillståndsändrande
    // anrop kommer från panelens egen origin.
    var verdict = PanelRequestGuard.Evaluate(
        new PanelRequestGuard.Request(
            context.Request.Method,
            context.Request.Path.Value ?? "/",
            context.Request.Host.Value,
            context.Connection.LocalPort,
            context.Request.Headers["Origin"].ToString(),
            context.Request.Headers["Sec-Fetch-Site"].ToString(),
            context.Request.Headers["X-CraftPilot-Token"].ToString()),
        session.Token);

    if (verdict != PanelRequestGuard.Verdict.Allow)
    {
        var unauthorized = verdict == PanelRequestGuard.Verdict.Unauthorized;
        context.Response.StatusCode = unauthorized
            ? StatusCodes.Status401Unauthorized
            : StatusCodes.Status403Forbidden;
        await context.Response.WriteAsJsonAsync(new
        {
            error = unauthorized ? "unauthorized" : "forbidden",
        });
        return;
    }

    await next();
});

app.MapGet("/", (HttpContext context) =>
{
    context.Response.Headers.CacheControl = "no-store";
    return Results.Content(indexHtml, "text/html; charset=utf-8", Encoding.UTF8);
});

app.MapGet("/app.css", (HttpContext context) =>
{
    context.Response.Headers.CacheControl = "no-store";
    return Results.Content(ReadResource("wwwroot.app.css"), "text/css; charset=utf-8", Encoding.UTF8);
});

app.MapGet("/app.js", (HttpContext context) =>
{
    context.Response.Headers.CacheControl = "no-store";
    return Results.Content(ReadResource("wwwroot.app.js"), "text/javascript; charset=utf-8", Encoding.UTF8);
});

// Panelens svenska och engelska texter.
app.MapGet("/i18n.js", (HttpContext context) =>
{
    context.Response.Headers.CacheControl = "no-store";
    return Results.Content(ReadResource("wwwroot.i18n.js"), "text/javascript; charset=utf-8", Encoding.UTF8);
});

app.MapGet("/api/status", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.GetStatusAsync(ct)));

app.MapGet("/api/logs", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(new { logs = await service.GetLogsAsync(ct) }));

app.MapPost("/api/install", async (
    InstallRequest request,
    AgentSetupService service,
    CancellationToken ct) => Results.Ok(await service.InstallAsync(request, ct)));

app.MapGet("/api/version", async (UpdateChecker checker, CancellationToken ct) =>
    Results.Ok(await checker.CheckAsync(false, ct)));

// Knappen Kontrollera nu: frågar GitHub direkt, oavsett valt intervall.
app.MapPost("/api/version/check", async (UpdateChecker checker, CancellationToken ct) =>
    Results.Ok(await checker.CheckAsync(true, ct)));

// Panelens egna inställningar (i dag intervallet för versionskontrollen).
app.MapGet("/api/settings", (PanelSettingsStore store) =>
    Results.Ok(new { updateCheckInterval = store.Load().UpdateCheckInterval }));

app.MapPost("/api/settings", (SettingsRequest request, PanelSettingsStore store) =>
    Results.Ok(new { updateCheckInterval = store.SaveInterval(request.UpdateCheckInterval).UpdateCheckInterval }));

app.MapPost("/api/update", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.UpdateAsync(ct)));

// Startar en redan installerad agent utan att hämta någon ny image.
app.MapPost("/api/start", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.StartAsync(ct)));

// Slår SFTP på eller av för noden: flaggan skrivs i .env.desktop och agenten
// återskapas, varpå den startar eller tar bort SFTPGo-containern.
app.MapPost("/api/sftp", async (SftpRequest request, AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.SetSftpEnabledAsync(request.Enabled, ct)));

app.MapPost("/api/stop", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.StopAsync(ct)));

app.MapPost("/api/remove", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.RemoveAsync(ct)));

// Avbryter en pågående docker-åtgärd (installera, uppdatera, stoppa, ta bort).
app.MapPost("/api/servers/prune", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.PruneServersAsync(ct)));

// Startar om agenten respektive en servercontainer. Server-id:t valideras
// mot Docker-listan i AgentSetupService; inget containernamn kommer från klienten.
app.MapPost("/api/restart", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.RestartAgentAsync(ct)));

app.MapPost("/api/servers/{serverId}/restart", async (
    string serverId,
    AgentSetupService service,
    CancellationToken ct) => Results.Ok(await service.RestartServerAsync(serverId, ct)));

// En redigerad textfil med versioner, Docker-läge, servrar och agentlogg.
app.MapGet("/api/diagnostics", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(new { text = await service.GetDiagnosticsAsync(ct) }));

// Hämtar Docker Desktops officiella installationsprogram till %TEMP%,
// verifierar Authenticode-signaturen och startar det. Panelen installerar
// aldrig Docker tyst; webbläsarlänken i panelen är kvar som fallback.
app.MapPost("/api/docker-install", async (DockerDesktopInstaller installer, CancellationToken ct) =>
    Results.Ok(await installer.DownloadAndStartAsync(ct)));

// Läget för Docker-nedladdningen, så rutan kan visa procent och hämtad storlek.
app.MapGet("/api/docker-install/progress", (DockerDesktopInstaller installer) =>
    Results.Ok(installer.Progress));

// Avbryter en pågående nedladdning och städar den halvfärdiga filen.
app.MapPost("/api/docker-install/cancel", (DockerDesktopInstaller installer) =>
    Results.Ok(new { cancelled = installer.Cancel() }));

// Läget för en pågående nedladdning, så baren kan visa procent.
app.MapGet("/api/panel-update/progress", (PanelUpdater updater) =>
    Results.Ok(updater.Progress));

// Hämtar panelens nästa exe från GitHub, kontrollerar SHA256, byter ut
// filen och startar om panelen. Den här processen stängs strax efter svaret.
app.MapPost("/api/panel-update", async (
    PanelUpdater updater,
    SetupSession setupSession,
    IHostApplicationLifetime lifetime,
    CancellationToken ct) =>
{
    if (setupSession.Busy)
    {
        return Results.Ok(new PanelUpdateResult(
            false,
            "Vänta tills den pågående åtgärden är klar.",
            null,
            null));
    }

    var result = await updater.UpdateAsync(ct);
    if (result.Ok)
    {
        _ = Task.Run(async () =>
        {
            await Task.Delay(700);
            lifetime.StopApplication();
        });
    }

    return Results.Ok(result);
});

app.MapPost("/api/cancel", (SetupSession session) =>
    Results.Ok(new { ok = session.Cancel() }));

app.MapPost("/api/open-folder", (AgentSetupService service) =>
{
    service.OpenInstallFolder();
    return Results.Ok(new { ok = true });
});

app.MapPost("/api/quit", (IHostApplicationLifetime lifetime) =>
{
    lifetime.StopApplication();
    return Results.Ok(new { ok = true });
});

app.MapPost("/api/startup", (StartupRequest request) =>
{
    try
    {
        StartupRegistration.SetEnabled(request.Enabled);
        return Results.Ok(new { ok = true, enabled = StartupRegistration.IsEnabled });
    }
    catch (Exception ex)
    {
        return Results.Ok(new { ok = false, error = ex.Message });
    }
});

await app.StartAsync();

var url = app.Urls.FirstOrDefault() ?? "http://127.0.0.1:5188";
Console.WriteLine($"CraftPilot Agent Setup UI: {url}");
Console.WriteLine("Panelen körs i trayen. Välj Avsluta i tray-menyn för att stoppa panelen.");

var setupService = app.Services.GetRequiredService<AgentSetupService>();
var updateChecker = app.Services.GetRequiredService<UpdateChecker>();
if (!WebViewWindow.TryStart(url, app.Lifetime, setupService, updateChecker, !startInTray))
{
    if (!startInTray)
    {
        WebViewWindow.ShowFallbackMessage();
        OpenBrowser(url);
    }
    else
    {
        Console.Error.WriteLine("Panelen kunde inte starta i trayen.");
    }
}

await app.WaitForShutdownAsync();

string ReadResource(string name)
{
    using var stream = assembly.GetManifestResourceStream(name)
        ?? throw new InvalidOperationException($"Resursen {name} saknas.");
    using var reader = new StreamReader(stream);
    return reader.ReadToEnd();
}

static void OpenBrowser(string url)
{
    try
    {
        if (OperatingSystem.IsWindows())
        {
            Process.Start(new ProcessStartInfo(url) { UseShellExecute = true });
        }
        else if (OperatingSystem.IsLinux())
        {
            Process.Start("xdg-open", url);
        }
        else
        {
            Process.Start("open", url);
        }
    }
    catch
    {
        // The URL is printed to the console; the user can open it manually.
    }
}

namespace CraftPilot.AgentSetup
{
    public sealed class SetupSession
    {
        private readonly object operationGate = new();
        private CancellationTokenSource? operation;

        public string Token { get; } = Convert.ToHexString(RandomNumberGenerator.GetBytes(16));
        public SemaphoreSlim OperationLock { get; } = new(1, 1);
        public volatile bool Busy;

        /// Startar en avbrytbar åtgärd. Tokenen avbryts av /api/cancel eller när
        /// anropet från panelen avbryts (t.ex. att fönstret stängs).
        public CancellationTokenSource BeginOperation(CancellationToken ct)
        {
            var linked = CancellationTokenSource.CreateLinkedTokenSource(ct);
            lock (operationGate)
            {
                operation = linked;
            }

            return linked;
        }

        public void EndOperation(CancellationTokenSource finished)
        {
            lock (operationGate)
            {
                if (ReferenceEquals(operation, finished))
                {
                    operation = null;
                }
            }

            finished.Dispose();
        }

        /// Avbryter den pågående åtgärden. False när inget kör.
        public bool Cancel()
        {
            lock (operationGate)
            {
                if (operation is null)
                {
                    return false;
                }

                operation.Cancel();
                return true;
            }
        }
    }

    public sealed record InstallRequest(string? MasterUrl, string? DataDir);

    public sealed record StartupRequest(bool Enabled);

    public sealed record SftpRequest(bool Enabled);

    public sealed record SettingsRequest(string? UpdateCheckInterval);
}
