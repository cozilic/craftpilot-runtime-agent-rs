using System.Net.Http;
using System.Net.Http.Headers;
using System.Text.Json;

namespace CraftPilot.AgentSetup;

public sealed class UpdateChecker
{
    private const string LatestReleaseUrl =
        "https://api.github.com/repos/cozilic/craftpilot-runtime-agent-rs/releases/latest";

    private static readonly TimeSpan CacheDuration = TimeSpan.FromMinutes(10);
    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(5) };

    private readonly SemaphoreSlim _gate = new(1, 1);
    private UpdateCheckResponse? _cached;
    private DateTimeOffset _cachedAt;

    public async Task<UpdateCheckResponse> CheckAsync(CancellationToken ct = default)
    {
        var current = PanelVersion.Current;
        if (TryGetCached(current, out var cached))
        {
            return cached;
        }

        await _gate.WaitAsync(ct);
        try
        {
            if (TryGetCached(current, out cached))
            {
                return cached;
            }

            var result = await FetchAsync(current, ct);
            if (result.Error is null)
            {
                _cached = result;
                _cachedAt = DateTimeOffset.UtcNow;
            }

            return result;
        }
        finally
        {
            _gate.Release();
        }
    }

    private bool TryGetCached(string current, out UpdateCheckResponse result)
    {
        if (_cached is { } cached &&
            string.Equals(cached.CurrentVersion, current, StringComparison.Ordinal) &&
            DateTimeOffset.UtcNow - _cachedAt < CacheDuration)
        {
            result = cached;
            return true;
        }

        result = null!;
        return false;
    }

    private static async Task<UpdateCheckResponse> FetchAsync(string current, CancellationToken ct)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, LatestReleaseUrl);
        request.Headers.UserAgent.ParseAdd($"CraftPilot-AgentSetup/{current}");
        request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/vnd.github+json"));
        request.Headers.Add("X-GitHub-Api-Version", "2022-11-28");

        try
        {
            using var response = await Http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct);
            if (!response.IsSuccessStatusCode)
            {
                return Error(current, $"GitHub svarade HTTP {(int)response.StatusCode}.");
            }

            await using var stream = await response.Content.ReadAsStreamAsync(ct);
            using var document = await JsonDocument.ParseAsync(stream, cancellationToken: ct);
            var root = document.RootElement;

            var tag = root.TryGetProperty("tag_name", out var tagValue)
                ? tagValue.GetString()
                : null;
            if (string.IsNullOrWhiteSpace(tag))
            {
                return Error(current, "GitHub-releasen saknar en versionstag.");
            }

            var releaseUrl = root.TryGetProperty("html_url", out var urlValue)
                ? urlValue.GetString()
                : null;
            if (!IsGitHubReleaseUrl(releaseUrl))
            {
                return Error(current, "GitHub-releasen har en ogiltig länk.");
            }

            return new UpdateCheckResponse
            {
                CurrentVersion = current,
                LatestVersion = tag,
                ReleaseUrl = releaseUrl,
                UpdateAvailable = ReleaseVersion.IsNewer(tag, current),
                CheckedAt = DateTimeOffset.UtcNow,
            };
        }
        catch (TaskCanceledException) when (!ct.IsCancellationRequested)
        {
            return Error(current, "GitHub svarade inte i tid.");
        }
        catch (HttpRequestException)
        {
            return Error(current, "Kunde inte nå GitHub.");
        }
        catch (JsonException)
        {
            return Error(current, "GitHub svarade med ett oväntat format.");
        }
    }

    private static bool IsGitHubReleaseUrl(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) &&
        uri.Scheme == Uri.UriSchemeHttps &&
        string.Equals(uri.Host, "github.com", StringComparison.OrdinalIgnoreCase) &&
        uri.AbsolutePath.StartsWith(
            "/cozilic/craftpilot-runtime-agent-rs/releases/",
            StringComparison.Ordinal);

    private static UpdateCheckResponse Error(string current, string error) => new()
    {
        CurrentVersion = current,
        Error = error,
        CheckedAt = DateTimeOffset.UtcNow,
    };
}
