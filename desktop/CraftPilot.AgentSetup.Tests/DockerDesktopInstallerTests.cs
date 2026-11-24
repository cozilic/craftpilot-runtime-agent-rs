using System.Net;
using System.Text;

namespace CraftPilot.AgentSetup.Tests;

// Panelen hämtar Docker Desktops installationsprogram och startar det med
// användarens behörigheter. Testerna låser att bara Dockers officiella adress
// godtas, att filen hamnar i %TEMP%, att inget startas innan signaturen är
// verifierad och att webbläsarlänken blir kvar som fallback vid fel.
public sealed class DockerDesktopInstallerTests : IDisposable
{
    private readonly string _temp = Path.Combine(Path.GetTempPath(), $"craftpilot-docker-{Guid.NewGuid():N}");

    public DockerDesktopInstallerTests()
    {
        Directory.CreateDirectory(_temp);
    }

    public void Dispose()
    {
        try
        {
            Directory.Delete(_temp, true);
        }
        catch (IOException)
        {
            // Testmappen städas av operativsystemet om en fil är låst.
        }
    }

    [Fact]
    public void Downloads_the_official_https_installer()
    {
        var uri = new Uri(DockerDesktopInstaller.DownloadUrl);

        Assert.Equal(Uri.UriSchemeHttps, uri.Scheme);
        Assert.Equal("desktop.docker.com", uri.Host);
        Assert.EndsWith(".exe", uri.AbsolutePath, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task Starts_the_installer_only_after_the_signature_is_verified()
    {
        var launched = new List<string>();
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("MZ fake installer")),
            verify: _ => true,
            launch: path =>
            {
                launched.Add(path);
                return true;
            });

        var result = await installer.DownloadAndStartAsync();

        Assert.True(result.Ok);
        var path = Assert.Single(launched);
        Assert.Equal(Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName), path);
        Assert.True(File.Exists(path));
    }

    [Fact]
    public async Task Deletes_the_download_and_starts_nothing_when_the_signature_is_rejected()
    {
        var launched = false;
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("not signed")),
            verify: _ => false,
            launch: _ =>
            {
                launched = true;
                return true;
            });

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(launched);
        Assert.Contains("Docker-signatur", result.Message);
        Assert.Contains("länken Hämta Docker Desktop", result.Message);
        Assert.False(File.Exists(Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName)));
    }

    [Theory]
    [InlineData("http://desktop.docker.com/Docker%20Desktop%20Installer.exe")]
    [InlineData("https://evil.example/Docker%20Desktop%20Installer.exe")]
    public async Task Refuses_a_download_that_ended_on_another_address(string finalUrl)
    {
        var launched = false;
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("MZ", finalUrl: finalUrl)),
            verify: _ => true,
            launch: _ =>
            {
                launched = true;
                return true;
            });

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(launched);
        Assert.Contains("oväntad adress", result.Message);
        Assert.False(File.Exists(Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName)));
    }

    [Fact]
    public async Task Reports_an_http_error_without_starting_anything()
    {
        var launched = false;
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("nope", HttpStatusCode.InternalServerError)),
            verify: _ => true,
            launch: _ =>
            {
                launched = true;
                return true;
            });

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(launched);
        Assert.Contains("HTTP 500", result.Message);
    }

    [Fact]
    public async Task Refuses_a_download_that_is_larger_than_expected()
    {
        var launched = false;
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("MZ", length: 2_000_000_000)),
            verify: _ => true,
            launch: _ =>
            {
                launched = true;
                return true;
            });

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(launched);
        Assert.Contains("större än väntat", result.Message);
    }

    [Fact]
    public async Task Keeps_the_verified_installer_when_the_start_is_cancelled()
    {
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("MZ")),
            verify: _ => true,
            launch: _ => false);

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.Contains("kunde inte startas", result.Message);
        Assert.True(File.Exists(Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName)));
    }

    [Fact]
    public async Task Refuses_a_second_download_while_one_is_running()
    {
        var entered = new TaskCompletionSource();
        var release = new TaskCompletionSource();
        var installer = CreateInstaller(
            async _ =>
            {
                entered.SetResult();
                await release.Task;
                return Respond("MZ");
            },
            verify: _ => true,
            launch: _ => true);

        var first = installer.DownloadAndStartAsync();
        await entered.Task;

        var second = await installer.DownloadAndStartAsync();

        Assert.False(second.Ok);
        Assert.Contains("redan", second.Message);

        release.SetResult();
        Assert.True((await first).Ok);
    }

    private DockerDesktopInstaller CreateInstaller(
        Func<HttpRequestMessage, Task<HttpResponseMessage>> respond,
        Func<string, bool> verify,
        Func<string, bool> launch) =>
        new(
            new HttpClient(new StubHandler(respond)) { Timeout = TimeSpan.FromMinutes(1) },
            _temp,
            verify,
            launch);

    private static HttpResponseMessage Respond(
        string body,
        HttpStatusCode status = HttpStatusCode.OK,
        string? finalUrl = null,
        long? length = null)
    {
        var content = new StringContent(body, Encoding.UTF8, "application/octet-stream");
        if (length is { } value)
        {
            content.Headers.ContentLength = value;
        }

        var response = new HttpResponseMessage(status) { Content = content };
        if (finalUrl is not null)
        {
            response.RequestMessage = new HttpRequestMessage(HttpMethod.Get, finalUrl);
        }

        return response;
    }

    private sealed class StubHandler(Func<HttpRequestMessage, Task<HttpResponseMessage>> respond) : HttpMessageHandler
    {
        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request,
            CancellationToken cancellationToken)
        {
            var response = await respond(request);
            response.RequestMessage ??= request;
            return response;
        }
    }
}
