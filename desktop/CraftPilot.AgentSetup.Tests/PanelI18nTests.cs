using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup.Tests;

// Panelens texter ligger i wwwroot/i18n.js. Testerna ser till att svenska och
// engelska har samma nycklar, samma platshållare och att HTML/app.js inte
// använder nycklar som saknas.
public sealed class PanelI18nTests
{
    private static readonly Regex LanguageStart = new(@"^\s{4}(sv|en):\s*\{", RegexOptions.Multiline);
    private static readonly Regex KeyValue = new(@"^\s{6}'([\w.]+)':\s*'(.*)',?\s*$", RegexOptions.Multiline);
    private static readonly Regex Placeholder = new(@"\{([A-Za-z]+)\}");
    private static readonly Regex HtmlKey = new("data-i18n(?:-[a-z-]+)?=\"([^\"]+)\"");
    private static readonly Regex ScriptKey = new(@"\bt\('([\w.]+)'");

    [Fact]
    public void Bada_spraken_har_samma_nycklar()
    {
        var (swedish, english) = ReadDictionaries();

        Assert.NotEmpty(swedish);
        Assert.NotEmpty(english);

        var missingInEnglish = swedish.Keys.Except(english.Keys).OrderBy(key => key).ToArray();
        var missingInSwedish = english.Keys.Except(swedish.Keys).OrderBy(key => key).ToArray();

        Assert.True(
            missingInEnglish.Length == 0 && missingInSwedish.Length == 0,
            $"Saknas i en: {string.Join(", ", missingInEnglish)} | Saknas i sv: {string.Join(", ", missingInSwedish)}");
    }

    [Fact]
    public void Bada_spraken_har_samma_platshallare()
    {
        var (swedish, english) = ReadDictionaries();

        foreach (var (key, swedishText) in swedish)
        {
            if (!english.TryGetValue(key, out var englishText))
            {
                continue;
            }

            var swedishPlaceholders = Placeholder.Matches(swedishText)
                .Select(match => match.Groups[1].Value)
                .OrderBy(name => name)
                .ToArray();
            var englishPlaceholders = Placeholder.Matches(englishText)
                .Select(match => match.Groups[1].Value)
                .OrderBy(name => name)
                .ToArray();

            Assert.True(
                swedishPlaceholders.SequenceEqual(englishPlaceholders),
                $"Platshållarna skiljer sig för {key}: sv [{string.Join(", ", swedishPlaceholders)}] en [{string.Join(", ", englishPlaceholders)}]");
        }
    }

    [Fact]
    public void Panelens_html_anvander_bara_nycklar_som_finns()
    {
        var (swedish, english) = ReadDictionaries();
        var html = File.ReadAllText(PanelFile("index.html"));
        var keys = HtmlKey.Matches(html).Select(match => match.Groups[1].Value).Distinct().ToArray();

        Assert.NotEmpty(keys);

        foreach (var key in keys)
        {
            Assert.True(swedish.ContainsKey(key), $"index.html använder {key} som saknas i sv");
            Assert.True(english.ContainsKey(key), $"index.html använder {key} som saknas i en");
        }
    }

    [Fact]
    public void Panelens_skript_anvander_bara_nycklar_som_finns()
    {
        var (swedish, english) = ReadDictionaries();
        var script = File.ReadAllText(PanelFile("app.js"));
        var keys = ScriptKey.Matches(script).Select(match => match.Groups[1].Value).Distinct().ToArray();

        Assert.NotEmpty(keys);

        foreach (var key in keys)
        {
            Assert.True(swedish.ContainsKey(key), $"app.js använder {key} som saknas i sv");
            Assert.True(english.ContainsKey(key), $"app.js använder {key} som saknas i en");
        }
    }

    private static (Dictionary<string, string> Swedish, Dictionary<string, string> English) ReadDictionaries()
    {
        var source = File.ReadAllText(PanelFile("i18n.js"));
        var swedish = new Dictionary<string, string>(StringComparer.Ordinal);
        var english = new Dictionary<string, string>(StringComparer.Ordinal);

        var language = string.Empty;
        foreach (var line in source.Split('\n'))
        {
            var start = LanguageStart.Match(line);
            if (start.Success)
            {
                language = start.Groups[1].Value;
                continue;
            }

            if (line.Contains("};", StringComparison.Ordinal))
            {
                language = string.Empty;
                continue;
            }

            var entry = KeyValue.Match(line);
            if (!entry.Success || language.Length == 0)
            {
                continue;
            }

            var target = language == "en" ? english : swedish;
            target[entry.Groups[1].Value] = entry.Groups[2].Value;
        }

        return (swedish, english);
    }

    // wwwroot ligger i projektet, inte i testets utdatamapp, så filen söks upp
    // uppåt från testkörningen.
    private static string PanelFile(string name)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            var candidate = Path.Combine(
                directory.FullName, "desktop", "CraftPilot.AgentSetup", "wwwroot", name);
            if (File.Exists(candidate))
            {
                return candidate;
            }

            directory = directory.Parent;
        }

        throw new FileNotFoundException($"Hittade inte desktop/CraftPilot.AgentSetup/wwwroot/{name}.");
    }
}
