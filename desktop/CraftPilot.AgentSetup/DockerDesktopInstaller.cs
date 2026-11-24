using System.ComponentModel;
using System.Diagnostics;
using System.Net.Http;

namespace CraftPilot.AgentSetup;

public sealed record DockerInstallResult(bool Ok, string Message);

/// <summary>
/// Hämtar Docker Desktops officiella installationsprogram till %TEMP%,
/// verifierar Authenticode-signaturen och startar det. Panelen installerar
/// aldrig Docker tyst: installationsprogrammet visar UAC, Docker:s villkor och
/// WSL2-val, och webbläsarlänken i panelen finns kvar som fallback.
/// </summary>
public sealed class DockerDesktopInstaller
{
    public const string DownloadUrl =
        "https://desktop.docker.com/win/main/amd64/Docker%20Desktop%20Installer.exe";
    public const string InstallerFileName = "Docker Desktop Installer.exe";
    public const string FallbackHint = "Använd länken Hämta Docker Desktop i panelen.";

    public const string CancelledMessage =
        "Hämtningen avbröts och den halvfärdiga filen togs bort. Inget startades.";

    /// <summary>Ändelsen på den halvfärdiga filen; den flyttas först när allt är hämtat.</summary>
    public const string PartialFileSuffix = ".part";

    private const string ExpectedHost = "desktop.docker.com";
    private const long MaxDownloadBytes = 1_600_000_000;

    private static readonly HttpClient DefaultHttp = new() { Timeout = TimeSpan.FromMinutes(15) };

    private readonly HttpClient _http;
    private readonly string _tempDirectory;
    private readonly Func<string, bool> _signatureIsTrusted;
    private readonly Func<string, bool> _startInstaller;

    private long _received;
    private long _total = -1;
    private int _downloading;
    private int _busy;
    private CancellationTokenSource? _cts;

    public DockerDesktopInstaller()
        : this(DefaultHttp, Path.GetTempPath(), DockerDesktopSignature.IsTrusted, StartInstaller)
    {
    }

    /// <summary>Testbar variant: nätverk, signaturkontroll och start injiceras.</summary>
    internal DockerDesktopInstaller(
        HttpClient http,
        string tempDirectory,
        Func<string, bool> signatureIsTrusted,
        Func<string, bool> startInstaller)
    {
        _http = http;
        _tempDirectory = tempDirectory;
        _signatureIsTrusted = signatureIsTrusted;
        _startInstaller = startInstaller;
    }

    /// <summary>Läget för en pågående nedladdning, så rutan kan visa procent och storlek.</summary>
    public DockerDownloadProgress Progress => new(
        Volatile.Read(ref _downloading) == 1,
        Interlocked.Read(ref _received),
        Interlocked.Read(ref _total));

    /// <summary>
    /// Avbryter en pågående hämtning. Den halvfärdiga filen städas av samma
    /// felväg som ett avbrott, så inget ligger kvar i %TEMP% mellan försök.
    /// </summary>
    public bool Cancel()
    {
        var cts = Volatile.Read(ref _cts);
        if (cts is null || cts.IsCancellationRequested)
        {
            return false;
        }

        try
        {
            cts.Cancel();
            return true;
        }
        catch (ObjectDisposedException)
        {
            // Hämtningen hann precis avsluta.
            return false;
        }
    }

    public async Task<DockerInstallResult> DownloadAndStartAsync(CancellationToken ct = default)
    {
        if (Interlocked.CompareExchange(ref _busy, 1, 0) != 0)
        {
            return new DockerInstallResult(false, "Panelen hämtar redan installationsprogrammet.");
        }

        // Panelen kan avbryta hämtningen med Cancel(); token länkas därför
        // ihop med anropets egen token.
        using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        Volatile.Write(ref _cts, cts);

        try
        {
            return await DownloadAndStartCoreAsync(cts.Token);
        }
        finally
        {
            Volatile.Write(ref _cts, null);
            Volatile.Write(ref _busy, 0);
        }
    }

    private async Task<DockerInstallResult> DownloadAndStartCoreAsync(CancellationToken ct)
    {
        if (!OperatingSystem.IsWindows())
        {
            return new DockerInstallResult(false, "Docker Desktop kan bara installeras från Windows-panelen.");
        }

        var path = Path.Combine(_tempDirectory, InstallerFileName);
        var partialPath = Path.Combine(_tempDirectory, InstallerFileName + PartialFileSuffix);

        // En halvfärdig fil från ett avbrutet eller kraschat försök ska aldrig ligga kvar.
        TryDelete(partialPath);

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, DownloadUrl);
            request.Headers.Add("User-Agent", $"CraftPilot-AgentSetup/{PanelVersion.Current}");
            using var response = await _http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct);

            if (!response.IsSuccessStatusCode)
            {
                return new DockerInstallResult(false, $"Docker svarade HTTP {(int)response.StatusCode}.");
            }

            // En omdirigering får inte flytta nedladdningen till en annan värd.
            var finalUri = response.RequestMessage?.RequestUri;
            if (finalUri is null ||
                finalUri.Scheme != Uri.UriSchemeHttps ||
                !string.Equals(finalUri.Host, ExpectedHost, StringComparison.OrdinalIgnoreCase))
            {
                return new DockerInstallResult(
                    false,
                    $"Nedladdningen kom från en oväntad adress. {FallbackHint}");
            }

            if (response.Content.Headers.ContentLength > MaxDownloadBytes)
            {
                return new DockerInstallResult(
                    false,
                    $"Installationsprogrammet är större än väntat. {FallbackHint}");
            }

            await DownloadToAsync(response, partialPath, ct);
            ct.ThrowIfCancellationRequested();

            // Först nu får filen sitt riktiga namn, så en avbruten hämtning aldrig
            // ser ut som ett färdigt installationsprogram.
            File.Move(partialPath, path, overwrite: true);

            if (!_signatureIsTrusted(path))
            {
                TryDelete(path);
                return new DockerInstallResult(
                    false,
                    $"Installationsprogrammet har inte en giltig Docker-signatur. {FallbackHint}");
            }

            if (!_startInstaller(path))
            {
                return new DockerInstallResult(
                    false,
                    $"Installationsprogrammet kunde inte startas. {FallbackHint}");
            }

            return new DockerInstallResult(
                true,
                "Installationsprogrammet har startat. Följ guiden och starta Docker Desktop när installationen är klar.");
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            TryDelete(partialPath);
            return new DockerInstallResult(false, CancelledMessage);
        }
        catch (Exception ex) when (ex is HttpRequestException or IOException or UnauthorizedAccessException or TaskCanceledException)
        {
            TryDelete(partialPath);
            return new DockerInstallResult(
                false,
                $"Kunde inte hämta installationsprogrammet: {ex.Message} {FallbackHint}");
        }
    }

    private async Task DownloadToAsync(HttpResponseMessage response, string path, CancellationToken ct)
    {
        await using var source = await response.Content.ReadAsStreamAsync(ct);
        await using var target = File.Create(path);

        Interlocked.Exchange(ref _received, 0);
        Interlocked.Exchange(ref _total, response.Content.Headers.ContentLength ?? -1);
        Volatile.Write(ref _downloading, 1);

        try
        {
            var buffer = new byte[81920];
            long received = 0;
            int read;
            while ((read = await source.ReadAsync(buffer, ct)) > 0)
            {
                received += read;
                if (received > MaxDownloadBytes)
                {
                    throw new InvalidDataException("Installationsprogrammet är större än väntat.");
                }

                await target.WriteAsync(buffer.AsMemory(0, read), ct);
                Interlocked.Exchange(ref _received, received);
            }
        }
        finally
        {
            Volatile.Write(ref _downloading, 0);
        }
    }

    /// <summary>Startar installationsprogrammet med UAC; användaren klickar igenom.</summary>
    private static bool StartInstaller(string path)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = path,
                UseShellExecute = true,
                Verb = "runas",
            });

            return process is not null;
        }
        catch (Win32Exception)
        {
            // Användaren avbröt UAC, eller filen kunde inte startas.
            return false;
        }
    }

    private static void TryDelete(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // En låst fil städas nästa gång panelen hämtar installationsprogrammet.
        }
    }
}
