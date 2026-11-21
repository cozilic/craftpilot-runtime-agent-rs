using System.Globalization;

namespace CraftPilot.AgentSetup;

/// <summary>CPU och minne för en servercontainer, läst ur ett enda docker stats-anrop.</summary>
public sealed record ContainerUsage(double? CpuPercent, long? MemoryUsedMb, long? MemoryLimitMb);

/// <summary>
/// Läser <c>docker stats --no-stream</c> för alla servercontainrar på en gång.
/// Panelen visar bara lokala siffror; masterns telemetri är kvar som källa för
/// det som händer i spelet.
/// </summary>
public static class ServerStats
{
    /// <summary>Namn, CPU och minne — inget fält innehåller pipe.</summary>
    public const string Format = "{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}";

    public static IReadOnlyDictionary<string, ContainerUsage> Parse(string? output)
    {
        var usage = new Dictionary<string, ContainerUsage>(StringComparer.Ordinal);
        if (string.IsNullOrWhiteSpace(output))
        {
            return usage;
        }

        foreach (var rawLine in output.Split('\n'))
        {
            var line = rawLine.Trim();
            if (line.Length == 0)
            {
                continue;
            }

            var parts = line.Split('|');
            if (parts.Length < 3)
            {
                continue;
            }

            var name = parts[0].Trim();
            if (name.Length == 0)
            {
                continue;
            }

            var (used, limit) = ParseMemory(parts[2]);
            usage[name] = new ContainerUsage(ParsePercent(parts[1]), used, limit);
        }

        return usage;
    }

    public static double? ParsePercent(string? value)
    {
        var text = (value ?? string.Empty).Trim().TrimEnd('%').Trim();
        if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var percent) ||
            !double.IsFinite(percent) ||
            percent < 0 ||
            percent > 10_000)
        {
            return null;
        }

        return Math.Round(percent, 2);
    }

    /// <summary>"123.4MiB / 1GiB" → (123, 1024) MiB.</summary>
    public static (long? Used, long? Limit) ParseMemory(string? value)
    {
        var parts = (value ?? string.Empty).Split('/');
        if (parts.Length == 0)
        {
            return (null, null);
        }

        var used = parts.Length > 0 ? ParseMebibytes(parts[0]) : null;
        var limit = parts.Length > 1 ? ParseMebibytes(parts[1]) : null;
        return (used, limit);
    }

    private static long? ParseMebibytes(string? value)
    {
        var text = (value ?? string.Empty).Trim();
        if (text.Length == 0)
        {
            return null;
        }

        var unitStart = 0;
        while (unitStart < text.Length &&
               (char.IsDigit(text[unitStart]) || text[unitStart] is '.' or ','))
        {
            unitStart++;
        }

        var number = text[..unitStart].Replace(',', '.');
        if (!double.TryParse(number, NumberStyles.Float, CultureInfo.InvariantCulture, out var amount) ||
            !double.IsFinite(amount) ||
            amount < 0)
        {
            return null;
        }

        var unit = text[unitStart..].Trim();
        var multiplier = unit switch
        {
            "B" => 1.0 / (1024 * 1024),
            "kB" or "KB" => 1000.0 / (1024 * 1024),
            "KiB" => 1.0 / 1024,
            "MiB" or "MB" or "" => 1.0,
            "GiB" or "GB" => 1024.0,
            "TiB" or "TB" => 1024.0 * 1024,
            _ => double.NaN,
        };

        if (!double.IsFinite(multiplier))
        {
            return null;
        }

        return (long)Math.Round(amount * multiplier, MidpointRounding.AwayFromZero);
    }
}
