using System.Diagnostics;
using Microsoft.Win32;

namespace CraftPilot.AgentSetup;

public static class StartupRegistration
{
    public const string ValueName = "CraftPilot Runtime Agent";
    private const string RunKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";

    public static bool IsEnabled
    {
        get
        {
            try
            {
                using var key = Registry.CurrentUser.OpenSubKey(RunKeyPath);
                return key?.GetValue(ValueName) is string value && !string.IsNullOrWhiteSpace(value);
            }
            catch
            {
                return false;
            }
        }
    }

    public static void SetEnabled(bool enabled)
    {
        using var key = Registry.CurrentUser.CreateSubKey(RunKeyPath, writable: true)
            ?? throw new InvalidOperationException("Kunde inte öppna Windows autostart-nyckel.");

        if (enabled)
        {
            key.SetValue(ValueName, BuildCommand(ExecutablePath()), RegistryValueKind.String);
        }
        else
        {
            key.DeleteValue(ValueName, throwOnMissingValue: false);
        }
    }

    public static bool IsStartupInvocation(IEnumerable<string> args) =>
        args.Any(arg =>
            string.Equals(arg, "--startup", StringComparison.OrdinalIgnoreCase) ||
            string.Equals(arg, "--tray", StringComparison.OrdinalIgnoreCase));

    public static string BuildCommand(string executablePath)
    {
        if (string.IsNullOrWhiteSpace(executablePath))
        {
            throw new ArgumentException("Sökvägen till panelen saknas.", nameof(executablePath));
        }

        return $"\"{Path.GetFullPath(executablePath)}\" --startup";
    }

    private static string ExecutablePath() =>
        Environment.ProcessPath
        ?? Process.GetCurrentProcess().MainModule?.FileName
        ?? throw new InvalidOperationException("Kunde inte hitta panelens sökväg.");
}
