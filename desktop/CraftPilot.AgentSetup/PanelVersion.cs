using System.Reflection;

namespace CraftPilot.AgentSetup;

public static class PanelVersion
{
    public static string Current { get; } = Resolve();

    private static string Resolve()
    {
        var informational = typeof(PanelVersion).Assembly
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?
            .InformationalVersion;

        var value = informational?.Split('+', 2)[0];
        return string.IsNullOrWhiteSpace(value) ? "0.0.0" : value;
    }
}
