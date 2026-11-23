using System.Globalization;
using System.Security.Cryptography;
using System.Text;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Vaktar panelens HTTP-yta. Servern lyssnar bara på loopback och varje
/// /api-anrop kräver körningens sessionsnyckel, men det räcker inte mot en
/// webbsida: en annan origin kan skicka POST hit, och DNS-rebinding kan låta
/// en angripares domän peka på 127.0.0.1. Därför avvisas också fel Host, fel
/// Origin och webbläsaranrop som inte är same-origin innan routen körs.
/// </summary>
public static class PanelRequestGuard
{
    public enum Verdict
    {
        Allow,
        Unauthorized,
        Forbidden,
    }

    /// <summary>Det vakten behöver veta om ett inkommande anrop.</summary>
    public readonly record struct Request(
        string Method,
        string Path,
        string? Host,
        int LocalPort,
        string? Origin,
        string? SecFetchSite,
        string? Token);

    public static Verdict Evaluate(Request request, string sessionToken)
    {
        // Host kontrolleras först och för alla sökvägar: sidan på "/" bär
        // sessionsnyckeln, så en rebindad domän får aldrig läsa den.
        if (!IsPanelHost(request.Host, request.LocalPort))
        {
            return Verdict.Forbidden;
        }

        if (IsApiPath(request.Path) && !TokenMatches(request.Token, sessionToken))
        {
            return Verdict.Unauthorized;
        }

        // Tillståndsändrande anrop får bara komma från panelens egen origin.
        // Anrop utan webbläsarhuvuden (curl, skript) släpps igenom: en
        // främmande sida kan inte skicka dem utan webbläsarens huvuden, och
        // de måste ändå ha sessionsnyckeln.
        if (IsStateChanging(request.Method) && !IsSameOriginRequest(request))
        {
            return Verdict.Forbidden;
        }

        return Verdict.Allow;
    }

    /// <summary>
    /// Panelen serveras från <c>http://127.0.0.1:&lt;port&gt;</c>; webbläsar-
    /// fallbacken kan använda <c>localhost</c>. Allt annat i Host-huvudet är
    /// en annan tjänst eller ett rebinding-försök.
    /// </summary>
    private static bool IsPanelHost(string? host, int localPort)
    {
        if (string.IsNullOrWhiteSpace(host) || localPort is <= 0 or > 65535)
        {
            return false;
        }

        var value = host.Trim();
        var port = localPort.ToString(CultureInfo.InvariantCulture);
        return string.Equals(value, $"127.0.0.1:{port}", StringComparison.OrdinalIgnoreCase) ||
            string.Equals(value, $"localhost:{port}", StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsApiPath(string path) =>
        path.Equals("/api", StringComparison.OrdinalIgnoreCase) ||
        path.StartsWith("/api/", StringComparison.OrdinalIgnoreCase);

    private static bool IsStateChanging(string method) =>
        method.Equals("POST", StringComparison.OrdinalIgnoreCase) ||
        method.Equals("PUT", StringComparison.OrdinalIgnoreCase) ||
        method.Equals("PATCH", StringComparison.OrdinalIgnoreCase) ||
        method.Equals("DELETE", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Saknas både Origin och Sec-Fetch-Site är anropet inte ett
    /// webbläsaranrop och släpps igenom (nyckelkravet gäller ändå).
    /// </summary>
    private static bool IsSameOriginRequest(Request request)
    {
        var origin = request.Origin?.Trim();
        if (!string.IsNullOrEmpty(origin) && !IsPanelOrigin(origin, request.LocalPort))
        {
            return false;
        }

        var site = request.SecFetchSite?.Trim();
        if (!string.IsNullOrEmpty(site) &&
            !site.Equals("same-origin", StringComparison.OrdinalIgnoreCase) &&
            !site.Equals("none", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        return true;
    }

    private static bool IsPanelOrigin(string origin, int localPort)
    {
        var port = localPort.ToString(CultureInfo.InvariantCulture);
        return string.Equals(origin, $"http://127.0.0.1:{port}", StringComparison.OrdinalIgnoreCase) ||
            string.Equals(origin, $"http://localhost:{port}", StringComparison.OrdinalIgnoreCase);
    }

    private static bool TokenMatches(string? provided, string expected)
    {
        if (string.IsNullOrEmpty(provided) || string.IsNullOrEmpty(expected))
        {
            return false;
        }

        var given = Encoding.UTF8.GetBytes(provided);
        var wanted = Encoding.UTF8.GetBytes(expected);
        return given.Length == wanted.Length && CryptographicOperations.FixedTimeEquals(given, wanted);
    }
}
