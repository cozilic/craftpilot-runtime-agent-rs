using System.Net;
using System.Text;

namespace CraftPilot.AgentSetup.Tests;

// Panelen hämtar Docker Desktops installationsprogram och startar det med
// användarens behörigheter. Testerna låser att bara Dockers officiella adress
// godtas, att filen hamnar i %TEMP%, att inget startas innan signaturen är
// verifierad, att rutan får procent och storlek och att en halvfärdig fil
// aldrig ligger kvar mellan försök.
public sealed class DockerDesktopInstallerTests : IDisposable
{
    private readonly string _temp = Path.Combine(Path.GetTempPath(), $"craftpilot-docker-{Guid.NewGuid():N}");

    public DockerDesktopInstallerTests()
    {
        Directory.CreateDirectory(_temp);
    }

    private string InstallerPath => Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName);

    private string PartialPath => Path.Combine(_temp, DockerDesktopInstaller.InstallerFileName + DockerDesktopInstaller.PartialFileSuffix);

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
        Assert.Equal(InstallerPath, path);
        Assert.True(File.Exists(path));
        Assert.False(File.Exists(PartialPath));
    }

    [Fact]
    public async Task Reports_percent_and_size_while_the_installer_downloads()
    {
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var stream = new GatedStream(Encoding.ASCII.GetBytes("MZ01234567"), release.Task);
        var installer = CreateInstaller(
            _ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StreamContent(stream) { Headers = { ContentLength = 10 } },
            }),
            verify: _ => true,
            launch: _ => true);

        var download = installer.DownloadAndStartAsync();

        await WaitUntilAsync(() => installer.Progress.Received > 0);
        var progress = installer.Progress;
        Assert.True(progress.Downloading);
        Assert.Equal(10, progress.Total);
        Assert.Equal(5, progress.Received);
        Assert.Equal(50, progress.Percent);

        // Den halvfärdiga filen har .part-namn och det färdiga namnet är ledigt.
        Assert.True(File.Exists(PartialPath));
        Assert.False(File.Exists(InstallerPath));

        release.SetResult();
        var result = await download;

        Assert.True(result.Ok);
        Assert.False(installer.Progress.Downloading);
        Assert.True(File.Exists(InstallerPath));
        Assert.False(File.Exists(PartialPath));
    }

    [Fact]
    public async Task Removes_a_partial_file_from_an_earlier_attempt()
    {
        File.WriteAllText(PartialPath, "halvfärdig");
        var installer = CreateInstaller(
            _ => Task.FromResult(Respond("nope", HttpStatusCode.BadGateway)),
            verify: _ => true,
            launch: _ => true);

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(File.Exists(PartialPath));
        Assert.False(File.Exists(InstallerPath));
    }

    [Fact]
    public async Task Removes_the_partial_file_when_the_download_breaks()
    {
        var installer = CreateInstaller(
            _ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StreamContent(new FailingStream(Encoding.ASCII.GetBytes("MZ01234567"))),
            }),
            verify: _ => true,
            launch: _ => true);

        var result = await installer.DownloadAndStartAsync();

        Assert.False(result.Ok);
        Assert.False(File.Exists(PartialPath));
        Assert.False(File.Exists(InstallerPath));
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
        Assert.False(File.Exists(InstallerPath));
        Assert.False(File.Exists(PartialPath));
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
        Assert.False(File.Exists(InstallerPath));
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
        Assert.True(File.Exists(InstallerPath));
        Assert.False(File.Exists(PartialPath));
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

    private static async Task WaitUntilAsync(Func<bool> condition)
    {
        var deadline = DateTime.UtcNow.AddSeconds(5);
        while (!condition())
        {
            if (DateTime.UtcNow > deadline)
            {
                throw new TimeoutException("Väntade för länge på nedladdningens progress.");
            }

            await Task.Delay(10);
        }
    }

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

    /// <summary>Skickar första biten direkt och resten först när testet släpper gaten.</summary>
    private sealed class GatedStream(byte[] data, Task gate) : Stream
    {
        private readonly TaskCompletionSource _firstRead = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private int _position;

        public Task FirstRead => _firstRead.Task;

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => data.Length;

        public override long Position
        {
            get => _position;
            set => throw new NotSupportedException();
        }

        public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
        {
            if (_position == 0)
            {
                var first = Math.Min(5, data.Length);
                data.AsSpan(0, first).CopyTo(buffer.Span);
                _position = first;
                _firstRead.TrySetResult();
                return first;
            }

            await gate.WaitAsync(cancellationToken);
            if (_position >= data.Length)
            {
                return 0;
            }

            var count = Math.Min(data.Length - _position, buffer.Length);
            data.AsSpan(_position, count).CopyTo(buffer.Span);
            _position += count;
            return count;
        }

        public override int Read(byte[] buffer, int offset, int count) =>
            ReadAsync(buffer.AsMemory(offset, count)).AsTask().GetAwaiter().GetResult();

        public override void Flush()
        {
        }

        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();

        public override void SetLength(long value) => throw new NotSupportedException();

        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }

    /// <summary>Ger några byte och kraschar sedan, som en bruten anslutning.</summary>
    private sealed class FailingStream(byte[] data) : Stream
    {
        private int _position;

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => data.Length;

        public override long Position
        {
            get => _position;
            set => throw new NotSupportedException();
        }

        public override int Read(byte[] buffer, int offset, int count)
        {
            if (_position >= data.Length)
            {
                throw new IOException("anslutningen bröts");
            }

            var size = Math.Min(data.Length - _position, count);
            data.AsSpan(_position, size).CopyTo(buffer.AsSpan(offset, size));
            _position += size;
            return size;
        }

        public override void Flush()
        {
        }

        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();

        public override void SetLength(long value) => throw new NotSupportedException();

        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }
}
