using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup;

public static partial class ClaimCodeParser
{
    [GeneratedRegex(@"\x1B\[[0-9;]*[A-Za-z]")]
    private static partial Regex AnsiRegex();

    [GeneratedRegex(@"claim_code=(CP-[A-Z0-9]{5}-[A-Z0-9]{5})")]
    private static partial Regex ClaimRegex();

    public static string? Extract(string? log)
    {
        if (string.IsNullOrEmpty(log))
        {
            return null;
        }

        var matches = ClaimRegex().Matches(StripAnsi(log));
        return matches.Count == 0 ? null : matches[^1].Groups[1].Value;
    }

    public static string StripAnsi(string value) => AnsiRegex().Replace(value, string.Empty);
}
