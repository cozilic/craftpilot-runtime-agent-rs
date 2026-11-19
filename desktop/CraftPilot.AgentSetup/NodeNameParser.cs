using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Agenten loggar <c>node_name=&lt;namn&gt;</c> när mastern skickar nodens namn i
/// claim-svaret. Panelen har ingen annan väg till namnet: det ligger i agentens
/// state-volym och kräver masterns autentisering. Loggen räcker — den är lokal
/// och namnet är samma som i panelen.
/// </summary>
public static partial class NodeNameParser
{
    [GeneratedRegex(@"node_name=(?<name>[^\r\n]+)\s*$", RegexOptions.Multiline)]
    private static partial Regex NameRegex();

    public static string? Extract(string? log)
    {
        if (string.IsNullOrEmpty(log))
        {
            return null;
        }

        var matches = NameRegex().Matches(ClaimCodeParser.StripAnsi(log));
        if (matches.Count == 0)
        {
            return null;
        }

        var name = matches[^1].Groups["name"].Value.Trim().Trim('"').Trim();
        return name.Length == 0 ? null : name;
    }
}
