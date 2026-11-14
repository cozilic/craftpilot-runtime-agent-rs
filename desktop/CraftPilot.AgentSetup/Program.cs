using System.Diagnostics;
using System.Net;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;

using CraftPilot.AgentSetup;

var builder = WebApplication.CreateBuilder(args);

// The dashboard controls a privileged Docker socket. Keep it on loopback and
// require a per-run token on every API call; the page gets the token inline.
builder.WebHost.ConfigureKestrel(options => options.Listen(IPAddress.Loopback, 0));
builder.Services.AddSingleton<SetupSession>();
builder.Services.AddSingleton<AgentSetupService>();
builder.Logging.ClearProviders();

var app = builder.Build();
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

app.MapPost("/api/update", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.UpdateAsync(ct)));

app.MapPost("/api/stop", async (AgentSetupService service, CancellationToken ct) =>
    Results.Ok(await service.StopAsync(ct)));

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

await app.StartAsync();

var url = app.Urls.FirstOrDefault() ?? "http://127.0.0.1:5188";
Console.WriteLine($"CraftPilot Agent Setup UI: {url}");
Console.WriteLine("Stäng det här fönstret för att stoppa panelen.");
OpenBrowser(url);

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
        public string Token { get; } = Convert.ToHexString(RandomNumberGenerator.GetBytes(16));
        public SemaphoreSlim OperationLock { get; } = new(1, 1);
        public volatile bool Busy;
    }

    public sealed record InstallRequest(string? MasterUrl, string? DataDir);
}
