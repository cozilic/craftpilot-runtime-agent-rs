using System.Globalization;

namespace CraftPilot.AgentSetup;

/// <summary>
/// En servercontainer som agenten har startat på den här datorn. Panelen läser
/// den direkt från Docker (labeln <c>craftpilot.managed</c>) i stället för från
/// mastern, så listan fungerar även när mastern är otillgänglig.
/// </summary>
public sealed record ServerContainer(string Name, string ServerId, string State, string Image)
{
    public bool Running => string.Equals(State, "running", StringComparison.OrdinalIgnoreCase);

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
        _ => CultureInfo.InvariantCulture.TextInfo.ToTitleCase(State),
    };
}

public static class ServerContainers
{
    /// <summary>
    /// <c>docker ps</c>-format med pipe som avgränsare: inget av fälten
    /// (namn, id, state, image) innehåller pipe.
    /// </summary>
    public const string Format = "{{.Names}}|{{.Label \"craftpilot.server_id\"}}|{{.State}}|{{.Image}}";

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
            if (parts.Length != 4)
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
                parts[3].Trim()));
        }

        return servers
            .OrderByDescending(server => server.Running)
            .ThenBy(server => server.Name, StringComparer.Ordinal)
            .ToList();
    }

    /// <summary>
    /// Containrar som panelen får städa bort: stoppade containrar med agentens
    /// eget namn. Körda containrar lämnas alltid i fred.
    /// </summary>
    public static IReadOnlyList<ServerContainer> Stopped(IEnumerable<ServerContainer> servers) =>
        servers
            .Where(server => !server.Running && IsManagedName(server.Name))
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
