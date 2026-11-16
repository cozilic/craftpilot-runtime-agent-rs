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
builder.Services.AddSingleton<UpdateChecker>();
builder.Services.AddSingleton<PanelUpdater>();
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
    if (context.Request.Path.StartsWithSegments("/api") &&
        !string.Equals(
            context.Request.Headers["X-CraftPilot-Token"].ToString(),
            session.Token,
            StringComparison.Ordinal))
    {
        context.Response.StatusCode = StatusCodes.Status401Unauthorized;
        await context.Response.WriteAsJsonAsync(new { error = "unauthorized" });
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

app.MapGet("/api/status", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.GetStatusAsync(ct)));

app.MapGet("/api/logs", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(new { logs = await service.GetLogsAsync(ct) }));

app.MapPost("/api/install", async (
    InstallRequest request,
    AgentSetupService service,
    CancellationToken ct) => Results.Ok(await service.InstallAsync(request, ct)));

app.MapGet("/api/version", async (UpdateChecker checker, CancellationToken ct) =>
    Results.Ok(await checker.CheckAsync(ct)));

app.MapPost("/api/update", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.UpdateAsync(ct)));

app.MapPost("/api/stop", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.StopAsync(ct)));

app.MapPost("/api/remove", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.RemoveAsync(ct)));

// Avbryter en pågående docker-åtgärd (installera, uppdatera, stoppa, ta bort).
app.MapPost("/api/servers/prune", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.PruneServersAsync(ct)));

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
if (!WebViewWindow.TryStart(url, app.Lifetime, setupService, !startInTray))
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
}
