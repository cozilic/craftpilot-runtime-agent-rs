namespace CraftPilot.AgentSetup;

public static class MasterUrl
{
    public const string Default = "https://craftpilot.se";

    public static bool TryNormalize(string? value, out string normalized, out string error)
    {
        normalized = string.Empty;
        error = string.Empty;

        var trimmed = (value ?? string.Empty).Trim();
        if (trimmed.Any(char.IsControl) ||
            !Uri.TryCreate(trimmed, UriKind.Absolute, out var uri) ||
            uri.Scheme != Uri.UriSchemeHttps)
        {
            error = "Ange en HTTPS-adress, till exempel https://craftpilot.se.";
            return false;
        }

        if (string.IsNullOrWhiteSpace(uri.Host) ||
            !string.IsNullOrEmpty(uri.UserInfo) ||
            !string.IsNullOrEmpty(uri.Query) ||
            !string.IsNullOrEmpty(uri.Fragment) ||
            uri.AbsolutePath is not ("" or "/"))
        {
            error = "Adressen måste vara en HTTPS-origin utan sökväg, query eller inloggningsuppgifter.";
            return false;
        }

        normalized = uri.GetLeftPart(UriPartial.Authority).TrimEnd('/');
        if (!string.Equals(normalized, Default, StringComparison.OrdinalIgnoreCase))
        {
            normalized = string.Empty;
            error = "Endast https://craftpilot.se kan användas.";
            return false;
        }

        return true;
    }
}
