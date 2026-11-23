namespace CraftPilot.AgentSetup.Tests;

// Panelen lyssnar på loopback, men det skyddar inte mot att en webbsida i
// användarens webbläsare postar till 127.0.0.1 eller att DNS-rebinding får en
// angripares domän att peka dit. Testerna låser vakten: rätt Host, nyckel på
// /api och same-origin för allt som ändrar tillstånd.
public sealed class PanelRequestGuardTests
{
    private const string Token = "8F3AC41D9B7E4C2A8F3AC41D9B7E4C2A";
    private const int Port = 5188;

    [Fact]
    public void Serves_the_panel_page_to_the_panel_origin()
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(path: "/"));
    }

    [Fact]
    public void Rejects_a_rebound_host_even_with_the_session_token()
    {
        // DNS-rebinding: angriparens domän pekar på 127.0.0.1, men Host-huvudet
        // bär fortfarande angriparens namn. Sidan på "/" får aldrig lämna ut
        // nyckeln, så det måste vara 403 även med rätt token.
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(path: "/", host: "evil.example:5188", token: Token));
    }

    [Theory]
    [InlineData("127.0.0.1:5000")]
    [InlineData("127.0.0.1")]
    [InlineData("localhost")]
    [InlineData("[::1]:5188")]
    [InlineData("192.168.1.10:5188")]
    [InlineData("")]
    public void Rejects_hosts_that_are_not_the_panel_loopback(string host)
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(path: "/api/status", host: host, token: Token));
    }

    [Fact]
    public void Accepts_the_localhost_alias_for_the_browser_fallback()
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(path: "/", host: "localhost:5188"));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("00000000000000000000000000000000")]
    [InlineData("8f3ac41d9b7e4c2a8f3ac41d9b7e4c2a")]
    public void Api_requires_the_session_token(string? token)
    {
        // Fel längd, fel värde och fel skiftläge är alla fel nyckel.
        Assert.Equal(
            PanelRequestGuard.Verdict.Unauthorized,
            Evaluate(path: "/api/status", token: token));
    }

    [Fact]
    public void Api_accepts_the_session_token()
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(path: "/api/status", token: Token));
    }

    [Theory]
    [InlineData("POST", "/api/remove")]
    [InlineData("POST", "/api/quit")]
    [InlineData("DELETE", "/api/servers/prune")]
    public void Rejects_state_changing_calls_from_another_origin(string method, string path)
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(method: method, path: path, token: Token, origin: "https://evil.example"));
    }

    [Fact]
    public void Rejects_state_changing_calls_the_browser_marks_as_cross_site()
    {
        // En webbsida som postar "enkelt" (text/plain) slipper CORS-preflight
        // och kan sakna Origin; Sec-Fetch-Site avslöjar den ändå.
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(
                method: "POST",
                path: "/api/remove",
                token: Token,
                site: "cross-site"));
    }

    [Fact]
    public void Rejects_another_local_port_as_same_site()
    {
        // En lokal dev-server på en annan port är same-site men inte
        // same-origin, och får inte styra panelen.
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(
                method: "POST",
                path: "/api/remove",
                token: Token,
                origin: "http://localhost:3000",
                site: "same-site"));
    }

    [Theory]
    [InlineData("http://127.0.0.1:5188", "same-origin")]
    [InlineData("http://localhost:5188", "none")]
    public void Accepts_state_changing_calls_from_the_panel_origin(string origin, string site)
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(
                method: "POST",
                path: "/api/remove",
                token: Token,
                origin: origin,
                site: site));
    }

    [Fact]
    public void Accepts_a_client_without_browser_headers()
    {
        // curl och skript har varken Origin eller Sec-Fetch-Site. De kan inte
        // lockas hit av en främmande sida och måste ändå ha nyckeln.
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(method: "POST", path: "/api/quit", token: Token));
    }

    [Theory]
    [InlineData("POST")]
    [InlineData("PUT")]
    [InlineData("PATCH")]
    [InlineData("DELETE")]
    public void All_state_changing_methods_are_origin_checked(string method)
    {
        Assert.Equal(
            PanelRequestGuard.Verdict.Forbidden,
            Evaluate(method: method, path: "/api/quit", token: Token, origin: "https://evil.example"));
    }

    [Fact]
    public void Reads_do_not_need_fetch_metadata()
    {
        // En cross-site GET utan nyckel får 401 av nyckelkravet, inte 403; och
        // GET ändrar inget, så fetch-huvudena spelar ingen roll här.
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(path: "/api/status", token: Token, origin: "https://evil.example"));
    }

    [Fact]
    public void Paths_that_only_look_like_the_api_are_not_protected_by_the_token()
    {
        // Samma gräns som tidigare middleware: /apiansokan är ingen API-väg.
        Assert.Equal(
            PanelRequestGuard.Verdict.Allow,
            Evaluate(path: "/apiansokan"));
    }

    [Fact]
    public void Middleware_uses_the_guard_and_binds_loopback()
    {
        var program = PanelSource("Program.cs");

        Assert.Contains("Listen(IPAddress.Loopback", program);
        Assert.Contains("PanelRequestGuard.Evaluate(", program);
        Assert.Contains("Status403Forbidden", program);
    }

    private static PanelRequestGuard.Verdict Evaluate(
        string method = "GET",
        string path = "/",
        string? host = "127.0.0.1:5188",
        string? origin = null,
        string? site = null,
        string? token = null) =>
        PanelRequestGuard.Evaluate(
            new PanelRequestGuard.Request(method, path, host, Port, origin, site, token),
            Token);

    private static string PanelSource(string name)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            var candidate = Path.Combine(directory.FullName, "desktop", "CraftPilot.AgentSetup", name);
            if (File.Exists(candidate))
            {
                return File.ReadAllText(candidate);
            }

            directory = directory.Parent;
        }

        throw new FileNotFoundException(name);
    }
}
