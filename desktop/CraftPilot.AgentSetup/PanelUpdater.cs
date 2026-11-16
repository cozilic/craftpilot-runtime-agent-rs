using System.Diagnostics;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text.Json;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Laddar ner och installerar panelens egen exe medan panelen körs. Windows
/// låser en körande exe, men filen får byta namn – därför flyttas den gamla
/// till <c>.old</c>, den nya läggs på plats och panelen startas om.
/// </summary>
public sealed class PanelUpdater
{
    public const string ExeFileName = "CraftPilot.AgentSetup.exe";
    public const string ChecksumsFileName = "SHA256SUMS";

    private const string LatestReleaseUrl =
        "https://api.github.com/repos/cozilic/craftpilot-runtime-agent-rs/releases/latest";
    private const string DownloadPathPrefix =
        "/cozilic/craftpilot-runtime-agent-rs/releases/download/";
    private const string ReleasePathPrefix =
        "/cozilic/craftpilot-runtime-agent-rs/releases/";

    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromMinutes(10) };

    public sealed record ReleaseFiles(
        string Tag,
        string InstallerUrl,
        string ChecksumsUrl,
        string ReleaseUrl);

    public async Task<PanelUpdateResult> UpdateAsync(CancellationToken ct = default)
    {
        var exePath = Environment.ProcessPath;
        if (!OperatingSystem.IsWindows() ||
            string.IsNullOrEmpty(exePath) ||
            !string.Equals(Path.GetFileName(exePath), ExeFileName, StringComparison.OrdinalIgnoreCase) ||
            !File.Exists(exePath))
        {
            return new PanelUpdateResult(
                false,
                "Panelen kan bara uppdatera sig själv när den körs som CraftPilot.AgentSetup.exe. Ladda ner filen manuellt.",
                null,
                null);
        }

        var folder = Path.GetDirectoryName(exePath)!;
        if (!FolderIsWritable(folder))
        {
            return new PanelUpdateResult(
                false,
                "Panelen ligger i en mapp som kräver administratörsbehörighet. Ladda ner den nya versionen manuellt.",
                null,
                null);
        }

        string json;
        try
        {
            using var request = CreateRequest(LatestReleaseUrl);
            using var response = await Http.SendAsync(request, ct);
            if (!response.IsSuccessStatusCode)
            {
                return new PanelUpdateResult(
                    false,
                    $"GitHub svarade HTTP {(int)response.StatusCode}.",
                    null,
                    null);
            }

            json = await response.Content.ReadAsStringAsync(ct);
        }
        catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException)
        {
            return new PanelUpdateResult(false, "Kunde inte nå GitHub.", null, null);
        }

        if (!TryParseRelease(json, out var release, out var parseError))
        {
            return new PanelUpdateResult(false, parseError, null, null);
        }

        if (!ReleaseVersion.IsNewer(release.Tag, PanelVersion.Current))
        {
            return new PanelUpdateResult(
                false,
                "Du kör redan den senaste versionen.",
                release.Tag,
                release.ReleaseUrl);
        }

        var directory = Path.Combine(Path.GetTempPath(), "craftpilot-panel-update");
        var downloadPath = Path.Combine(directory, ExeFileName);

        try
        {
            Directory.CreateDirectory(directory);
            var checksums = await GetStringAsync(release.ChecksumsUrl, ct);
            var expected = ParseChecksum(checksums, ExeFileName);
            if (expected is null)
            {
                return new PanelUpdateResult(
                    false,
                    "SHA256SUMS saknar panelens fil.",
                    release.Tag,
                    release.ReleaseUrl);
            }

            var actual = await DownloadAsync(release.InstallerUrl, downloadPath, ct);
            if (!string.Equals(expected, actual, StringComparison.OrdinalIgnoreCase))
            {
                TryDelete(downloadPath);
                return new PanelUpdateResult(
                    false,
                    "Kontrollsumman stämde inte, filen laddades inte ner.",
                    release.Tag,
                    release.ReleaseUrl);
            }
        }
        catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException or IOException or UnauthorizedAccessException)
        {
            TryDelete(downloadPath);
            return new PanelUpdateResult(
                false,
                $"Nedladdningen misslyckades: {ex.Message}",
                release.Tag,
                release.ReleaseUrl);
        }

        try
        {
            if (!LooksLikeWindowsExe(downloadPath))
            {
                TryDelete(downloadPath);
                return new PanelUpdateResult(
                    false,
                    "Filen som laddades ner är inte en Windows-exe.",
                    release.Tag,
                    release.ReleaseUrl);
            }
        }
        catch (IOException ex)
        {
            TryDelete(downloadPath);
            return new PanelUpdateResult(
                false,
                $"Kunde inte läsa den nedladdade filen: {ex.Message}",
                release.Tag,
                release.ReleaseUrl);
        }

        // Panelen körs från den här filen. Den får byta namn men inte raderas,
        // så den gamla versionen flyttas undan och den nya läggs på plats.
        var backupPath = exePath + ".old";
        try
        {
            TryDelete(backupPath);
            File.Move(exePath, backupPath);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return new PanelUpdateResult(
                false,
                $"Kunde inte byta ut panelens fil: {ex.Message}",
                release.Tag,
                release.ReleaseUrl);
        }

        try
        {
            File.Move(downloadPath, exePath);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            try
            {
                File.Move(backupPath, exePath);
            }
            catch
            {
                // Den gamla filen ligger kvar som .old bredvid panelen.
            }

            return new PanelUpdateResult(
                false,
                $"Kunde inte lägga den nya versionen på plats: {ex.Message}",
                release.Tag,
                release.ReleaseUrl);
        }

        try
        {
            Process.Start(new ProcessStartInfo(exePath)
            {
                UseShellExecute = true,
                WorkingDirectory = folder,
            });
        }
        catch (Exception ex) when (ex is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            return new PanelUpdateResult(
                false,
                $"Versionen är installerad men panelen kunde inte startas om: {ex.Message}",
                release.Tag,
                release.ReleaseUrl);
        }

        return new PanelUpdateResult(true, null, release.Tag, release.ReleaseUrl);
    }

    /// <summary>
    /// Läser version, exe och SHA256SUMS ur GitHubs release-svar. Bara filer
    /// som ligger i det här repots release-mapp godtas.
    /// </summary>
    public static bool TryParseRelease(string? json, out ReleaseFiles release, out string error)
    {
        release = null!;
        error = "GitHub svarade med ett oväntat format.";
        if (string.IsNullOrWhiteSpace(json))
        {
            return false;
        }

        try
        {
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            var tag = root.TryGetProperty("tag_name", out var tagValue) ? tagValue.GetString() : null;
            var releaseUrl = root.TryGetProperty("html_url", out var urlValue) ? urlValue.GetString() : null;
            if (string.IsNullOrWhiteSpace(tag) || !IsGitHubReleaseUrl(releaseUrl))
            {
                error = "GitHub-releasen saknar version eller länk.";
                return false;
            }

            string? installer = null;
            string? checksums = null;
            if (root.TryGetProperty("assets", out var assets) && assets.ValueKind == JsonValueKind.Array)
            {
                foreach (var asset in assets.EnumerateArray())
                {
                    var name = asset.TryGetProperty("name", out var nameValue) ? nameValue.GetString() : null;
                    var url = asset.TryGetProperty("browser_download_url", out var downloadValue)
                        ? downloadValue.GetString()
                        : null;
                    if (!IsGitHubDownloadUrl(url))
                    {
                        continue;
                    }

                    if (string.Equals(name, ExeFileName, StringComparison.Ordinal))
                    {
                        installer = url;
                    }
                    else if (string.Equals(name, ChecksumsFileName, StringComparison.Ordinal))
                    {
                        checksums = url;
                    }
                }
            }

            if (installer is null || checksums is null)
            {
                error = "Releasen saknar panelens exe eller SHA256SUMS.";
                return false;
            }

            release = new ReleaseFiles(tag!, installer, checksums, releaseUrl!);
            error = string.Empty;
            return true;
        }
        catch (JsonException)
        {
            return false;
        }
    }

    /// <summary>SHA256 för <paramref name="fileName"/> ur en SHA256SUMS-fil.</summary>
    public static string? ParseChecksum(string? checksums, string fileName)
    {
        if (string.IsNullOrWhiteSpace(checksums))
        {
            return null;
        }

        foreach (var line in checksums.Split('\n'))
        {
            var parts = line.Trim().Split(' ', StringSplitOptions.RemoveEmptyEntries);
            if (parts.Length < 2)
            {
                continue;
            }

            // GNU sha256sum markerar binärläge med * framför filnamnet.
            var name = parts[1].TrimStart('*');
            if (!string.Equals(name, fileName, StringComparison.Ordinal))
            {
                continue;
            }

            var hash = parts[0].Trim().ToLowerInvariant();
            if (hash.Length == 64 && hash.All(Uri.IsHexDigit))
            {
                return hash;
            }
        }

        return null;
    }

    private static HttpRequestMessage CreateRequest(string url)
    {
        var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Add("User-Agent", $"CraftPilot-AgentSetup/{PanelVersion.Current}");
        request.Headers.Add("Accept", "application/vnd.github+json, application/octet-stream");
        return request;
    }

    private static async Task<string> GetStringAsync(string url, CancellationToken ct)
    {
        using var request = CreateRequest(url);
        using var response = await Http.SendAsync(request, ct);
        response.EnsureSuccessStatusCode();
        return await response.Content.ReadAsStringAsync(ct);
    }

    /// <summary>Laddar ner filen och räknar ut SHA256 medan den skrivs.</summary>
    private static async Task<string> DownloadAsync(string url, string destination, CancellationToken ct)
    {
        using var request = CreateRequest(url);
        using var response = await Http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct);
        response.EnsureSuccessStatusCode();

        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        await using (var source = await response.Content.ReadAsStreamAsync(ct))
        await using (var target = File.Create(destination))
        {
            var buffer = new byte[81920];
            int read;
            while ((read = await source.ReadAsync(buffer, ct)) > 0)
            {
                hash.AppendData(buffer, 0, read);
                await target.WriteAsync(buffer.AsMemory(0, read), ct);
            }
        }

        return Convert.ToHexString(hash.GetHashAndReset()).ToLowerInvariant();
    }

    private static bool LooksLikeWindowsExe(string path)
    {
        using var stream = File.OpenRead(path);
        Span<byte> header = stackalloc byte[2];
        return stream.Read(header) == 2 && header[0] == (byte)'M' && header[1] == (byte)'Z';
    }

    private static bool FolderIsWritable(string folder)
    {
        try
        {
            var probe = Path.Combine(folder, $".craftpilot-{Guid.NewGuid():N}.tmp");
            using (File.Create(probe))
            {
            }

            File.Delete(probe);
            return true;
        }
        catch
        {
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
        catch
        {
            // En kvarglömd fil i temp-mappen är inte värd att fälla uppdateringen.
        }
    }

    private static bool IsGitHubReleaseUrl(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) &&
        uri.Scheme == Uri.UriSchemeHttps &&
        string.Equals(uri.Host, "github.com", StringComparison.OrdinalIgnoreCase) &&
        uri.AbsolutePath.StartsWith(ReleasePathPrefix, StringComparison.Ordinal);

    private static bool IsGitHubDownloadUrl(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) &&
        uri.Scheme == Uri.UriSchemeHttps &&
        string.Equals(uri.Host, "github.com", StringComparison.OrdinalIgnoreCase) &&
        uri.AbsolutePath.StartsWith(DownloadPathPrefix, StringComparison.Ordinal);
}
