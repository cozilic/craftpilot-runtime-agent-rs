using System.ComponentModel;
using System.Diagnostics;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Startar ett redan installerat Docker Desktop. Panelen startar bara filen i
/// Dockers vanliga installationsmappar och kör den utan UAC.
/// </summary>
public static class DockerDesktopLauncher
{
    public static bool Start() => Start(DockerDesktopLocator.Find(), Launch);

    internal static bool Start(string? path, Func<string, bool> launch) =>
        path is not null && launch(path);

    internal static bool Launch(string path)
    {
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = path,
                UseShellExecute = true,
            });

            return true;
        }
        catch (Exception ex) when (ex is Win32Exception or FileNotFoundException)
        {
            return false;
        }
    }
}
