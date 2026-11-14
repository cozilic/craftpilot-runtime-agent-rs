namespace CraftPilot.AgentSetup;

public readonly record struct ReleaseVersion(int Major, int Minor, int Build, int Revision)
    : IComparable<ReleaseVersion>
{
    public static bool TryParse(string? value, out ReleaseVersion version)
    {
        version = default;
        if (string.IsNullOrWhiteSpace(value))
        {
            return false;
        }

        var text = value.Trim();
        if (text.Length > 0 && (text[0] == 'v' || text[0] == 'V'))
        {
            text = text[1..];
        }

        // GitHub's /releases/latest endpoint only returns stable releases.
        // Ignore build metadata and prerelease suffixes when comparing the core version.
        var metadata = text.IndexOf('+');
        if (metadata >= 0)
        {
            text = text[..metadata];
        }

        var prerelease = text.IndexOf('-');
        if (prerelease >= 0)
        {
            text = text[..prerelease];
        }

        if (!Version.TryParse(text, out var parsed))
        {
            return false;
        }

        version = new ReleaseVersion(
            parsed.Major,
            parsed.Minor,
            Math.Max(parsed.Build, 0),
            Math.Max(parsed.Revision, 0));
        return true;
    }

    public static bool IsNewer(string? latest, string? current) =>
        TryParse(latest, out var latestVersion) &&
        TryParse(current, out var currentVersion) &&
        latestVersion.CompareTo(currentVersion) > 0;

    public int CompareTo(ReleaseVersion other)
    {
        var result = Major.CompareTo(other.Major);
        if (result != 0)
        {
            return result;
        }

        result = Minor.CompareTo(other.Minor);
        if (result != 0)
        {
            return result;
        }

        result = Build.CompareTo(other.Build);
        return result != 0 ? result : Revision.CompareTo(other.Revision);
    }
}
