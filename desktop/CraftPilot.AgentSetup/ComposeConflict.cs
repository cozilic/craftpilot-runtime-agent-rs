namespace CraftPilot.AgentSetup;

/// Compose vägrar återanvända ett container-namn som skapats av ett annat
/// projekt. Den gamla containern är ofarlig att ta bort (den ägs av panelen),
/// så panelen lagar krocken i stället för att fastna på den.
public static class ComposeConflict
{
    private const string Marker = "is already in use by container";

    public static bool IsNameConflict(string output) =>
        output.Contains(Marker, StringComparison.OrdinalIgnoreCase);
}
