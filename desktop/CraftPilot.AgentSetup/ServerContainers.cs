using System.Globalization;
using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup;

/// <summary>
/// En servercontainer som agenten har startat på den här datorn. Panelen läser
/// den direkt från Docker (labeln <c>craftpilot.managed</c>) i stället för från
/// mastern, så listan fungerar även när mastern är otillgänglig.
/// </summary>
public sealed record ServerContainer(
    string Name,
    string ServerId,
    string State,
    string Image,
    int? HostPort = null,
    int? ExitCode = null,
    double? CpuPercent = null,
    long? MemoryUsedMb = null,
    long? MemoryLimitMb = null)
{
    public bool Running => string.Equals(State, "running", StringComparison.OrdinalIgnoreCase);

    /// <summary>Servern finns i serverdata-mappen men har ingen container ännu.</summary>
    public bool WaitingForFirstStart => string.Equals(State, "absent", StringComparison.OrdinalIgnoreCase);

    /// <summary>Kort id, samma som agenten använder i containernamnet.</summary>
    public string ShortId => ServerId.Length >= 8 ? ServerId[..8] : ServerId;

    /// <summary>Människovänlig status för panelen (okänt läge visas som det är).</summary>
    public string StateLabel => State switch
    {
        "running" => "Körs",
        "exited" => "Stoppad",
        "created" => "Skapad",
        "restarting" => "Startar om",
        "paused" => "Pausad",
        "dead" => "Kraschad",
        "absent" => "Inte startad",
        _ => CultureInfo.InvariantCulture.TextInfo.ToTitleCase(State),
    };
}

public static class ServerContainers
{
    /// <summary>
    /// <c>docker ps</c>-format med pipe som avgränsare: inget av fälten
    /// (namn, id, state, image) innehåller pipe.
    /// </summary>
    public const string Format =
        "{{.Names}}|{{.Label \"craftpilot.server_id\"}}|{{.State}}|{{.Image}}|{{.Ports}}|{{.Status}}";

    private static readonly Regex ExitCodePattern = new(
        @"\((\d+)\)",
        RegexOptions.CultureInvariant);

    /// <summary>
    /// Namnet agenten ger serverns container: <c>craftpilot-</c> plus åtta
    /// hextecken ur server-id:t. Panelen använder det för att bara starta om
    /// en container som bevisligen hör till det id som anropet gäller.
    /// </summary>
    public static string ExpectedName(string? serverId) =>
        serverId is { Length: >= 8 } && Guid.TryParse(serverId, out _)
            ? $"craftpilot-{serverId[..8].ToLowerInvariant()}"
            : string.Empty;

    /// <summary>Exitkoden ur Docker-statusen, t.ex. "Exited (1) 2 minutes ago".</summary>
    public static int? ExitCodeFrom(string? status)
    {
        if (string.IsNullOrWhiteSpace(status))
        {
            return null;
        }

        var match = ExitCodePattern.Match(status);
        return match.Success && int.TryParse(match.Groups[1].Value, out var code) ? code : null;
    }

    public static IReadOnlyList<ServerContainer> Parse(string? output)
    {
        if (string.IsNullOrWhiteSpace(output))
        {
            return [];
        }

        var servers = new List<ServerContainer>();
        foreach (var rawLine in output.Split('\n'))
        {
            var line = rawLine.Trim();
            if (line.Length == 0)
            {
                continue;
            }

            var parts = line.Split('|');
            if (parts.Length < 4)
            {
                continue;
            }

            var name = parts[0].Trim();
            var serverId = parts[1].Trim();
            if (name.Length == 0 || serverId.Length == 0)
            {
                // Utan label vet panelen inte vilken server det är: hoppa över.
                continue;
            }

            servers.Add(new ServerContainer(
                name,
                serverId,
                parts[2].Trim().ToLowerInvariant(),
                parts[3].Trim(),
                parts.Length > 4 ? HostPortFrom(parts[4]) : null,
                parts.Length > 5 ? ExitCodeFrom(parts[5]) : null));
        }

        return Sorted(servers);
    }

    /// <summary>
    /// Lägger till de servrar som finns i serverdata-mappen men saknar
    /// container. En nyskapad server får sin katalog av den första
    /// snapshoten, långt innan den startas — utan de katalogerna visade
    /// panelen ingenting förrän servern hade startats och fått en container
    /// (2026-11-19).
    /// </summary>
    public static IReadOnlyList<ServerContainer> WithServerDirectories(
        IEnumerable<ServerContainer> containers,
        string? dataDir)
    {
        var servers = containers.ToList();

        if (string.IsNullOrWhiteSpace(dataDir) || !Directory.Exists(dataDir))
        {
            return Sorted(servers);
        }

        var known = new HashSet<string>(
            servers.Select(server => server.ServerId),
            StringComparer.OrdinalIgnoreCase);

        foreach (var directory in Directory.EnumerateDirectories(dataDir))
        {
            var serverId = Path.GetFileName(directory);
            if (!Guid.TryParse(serverId, out _) || !known.Add(serverId))
            {
                continue;
            }

            servers.Add(new ServerContainer(string.Empty, serverId, "absent", string.Empty));
        }

        return Sorted(servers);
    }

    /// <summary>Värdporten som containern publicerar, för server-list-ping.</summary>
    private static int? HostPortFrom(string ports)
    {
        foreach (var mapping in ports.Split(','))
        {
            var arrow = mapping.IndexOf("->", StringComparison.Ordinal);
            if (arrow < 0)
            {
                continue;
            }

            var left = mapping[..arrow].Trim();
            var colon = left.LastIndexOf(':');
            if (colon >= 0 && int.TryParse(left[(colon + 1)..], out var port) && port is > 0 and <= 65535)
            {
                return port;
            }
        }

        return null;
    }

    private static List<ServerContainer> Sorted(IEnumerable<ServerContainer> servers) =>
        servers
            .OrderByDescending(server => server.Running)
            .ThenBy(server => server.ShortId, StringComparer.Ordinal)
            .ToList();

    /// <summary>
    /// Containrar som panelen får städa bort: stoppade containrar med agentens
    /// eget namn. Körda containrar lämnas alltid i fred.
    /// </summary>
    public static IReadOnlyList<ServerContainer> Stopped(IEnumerable<ServerContainer> servers) =>
        servers
            .Where(server => !server.Running && !server.WaitingForFirstStart && IsManagedName(server.Name))
            .ToList();

    /// <summary>
    /// Agenten döper containrar till <c>craftpilot-</c> plus åtta hextecken.
    /// Panelen rör bara namn i den formen.
    /// </summary>
    public static bool IsManagedName(string? name) =>
        name is { Length: 19 } &&
        name.StartsWith("craftpilot-", StringComparison.Ordinal) &&
        name[11..].All(character => character is >= '0' and <= '9' or >= 'a' and <= 'f');
}
