namespace CraftPilot.AgentSetup;

public static class PanelNavigation
{
    public static bool IsLocalPanelUri(string? candidate, string panelUrl)
    {
        if (!Uri.TryCreate(candidate, UriKind.Absolute, out var uri) ||
            !Uri.TryCreate(panelUrl, UriKind.Absolute, out var panel))
        {
            return false;
        }

        return string.Equals(uri.Scheme, Uri.UriSchemeHttp, StringComparison.OrdinalIgnoreCase) &&
            string.Equals(panel.Scheme, Uri.UriSchemeHttp, StringComparison.OrdinalIgnoreCase) &&
            string.Equals(uri.Host, panel.Host, StringComparison.OrdinalIgnoreCase) &&
            uri.Port == panel.Port;
    }

    public static bool IsSafeExternalUri(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) &&
        (uri.Scheme == Uri.UriSchemeHttps || uri.Scheme == Uri.UriSchemeHttp);
}
