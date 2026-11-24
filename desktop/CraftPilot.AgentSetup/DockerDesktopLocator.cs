namespace CraftPilot.AgentSetup;

/// <summary>
/// Hittar Docker Desktops installationsfil bland de vanliga platserna. Samma
/// sökväg avgör om panelen erbjuder installation eller en startknapp.
/// </summary>
public static class DockerDesktopLocator
{
    public static string? Find()
    {
        if (!OperatingSystem.IsWindows())
        {
            return null;
        }

        return Find(
            File.Exists,
            Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));
    }

    internal static string? Find(
        Func<string, bool> exists,
        string programFiles,
        string localAppData)
    {
        var candidates = new[]
        {
            Path.Combine(programFiles, "Docker", "Docker", "Docker Desktop.exe"),
            Path.Combine(localAppData, "Docker", "Docker Desktop.exe"),
            Path.Combine(localAppData, "Programs", "Docker", "Docker Desktop.exe"),
        };

        return candidates.FirstOrDefault(exists);
    }
}
