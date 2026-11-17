using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup;

public static partial class ClaimCodeParser
{
    /// <summary>Raden agenten loggar när credentialen är hämtad och noden ansluten.</summary>
    public const string CompletedMarker = "enrollment completed";

    [GeneratedRegex(@"\x1B\[[0-9;]*[A-Za-z]")]
    private static partial Regex AnsiRegex();

    [GeneratedRegex(@"claim_code=(CP-[A-Z0-9]{5}-[A-Z0-9]{5})")]
    private static partial Regex ClaimRegex();

    /// <summary>
    /// Koden som fortfarande väntar på att lösas in.
    ///
    /// En nod som tas bort i panelen får agenten att anmäla sig igen och skriva
    /// ut en ny kod. Loggen innehåller då fortfarande den gamla raden "enrollment
    /// completed", så ordningen avgör: en kod som skrivits ut efter den senaste
    /// slutförda anslutningen gäller, medan en kod före den är förbrukad.
    /// </summary>
    public static string? ExtractPending(string? log)
    {
        if (string.IsNullOrEmpty(log))
        {
            return null;
        }

        var text = StripAnsi(log);
        var matches = ClaimRegex().Matches(text);
        if (matches.Count == 0)
        {
            return null;
        }

        var latest = matches[^1];
        var completedAt = text.LastIndexOf(CompletedMarker, StringComparison.OrdinalIgnoreCase);
        if (completedAt >= 0 && latest.Index < completedAt)
        {
            return null;
        }

        return latest.Groups[1].Value;
    }

    public static string StripAnsi(string value) => AnsiRegex().Replace(value, string.Empty);
}
