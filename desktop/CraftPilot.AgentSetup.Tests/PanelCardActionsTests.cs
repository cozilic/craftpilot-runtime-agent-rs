using System.Text.RegularExpressions;

namespace CraftPilot.AgentSetup.Tests;

// Starta/Stoppa-knapparna på AGENT- och SFTP-korten. Ett knapp-id utan
// hanterare, eller en hanterare mot fel endpoint, syns först när någon klickar
// i panelen — därför kontrolleras både markeringen och bindningen här.
public sealed class PanelCardActionsTests
{
    [Theory]
    [InlineData("agent-start", "/api/start")]
    [InlineData("agent-stop", "/api/stop")]
    [InlineData("sftp-start", "/api/sftp")]
    [InlineData("sftp-stop", "/api/sftp")]
    public void Card_button_is_wired_to_its_endpoint(string id, string endpoint)
    {
        var script = PanelFile("app.js");
        var handler = Regex.Match(
            script,
            $@"\$\('{Regex.Escape(id)}'\)\.addEventListener\('click',[\s\S]*?\n\}}\);");

        Assert.True(handler.Success, $"{id} saknar klickhanterare i app.js");
        Assert.Contains(endpoint, handler.Value);
    }

    [Fact]
    public void Sftp_buttons_require_a_running_agent()
    {
        var script = PanelFile("app.js");

        // SFTPGo startas och stoppas av agenten, så knapparna får inte vara
        // aktiva när agenten är stoppad — då skulle de starta den.
        Assert.Contains(
            "$('sftp-start').disabled = state.busy || !state.dockerReady || !agentRunning || sftpEnabled;",
            script);
        Assert.Contains(
            "$('sftp-stop').disabled = state.busy || !state.dockerReady || !agentRunning || !sftpEnabled;",
            script);
    }

    [Theory]
    [InlineData("agent-start")]
    [InlineData("agent-stop")]
    [InlineData("sftp-start")]
    [InlineData("sftp-stop")]
    public void Card_button_exists_in_the_markup(string id)
    {
        Assert.Contains($"id=\"{id}\"", PanelFile("index.html"));
    }

    private static string PanelFile(string name)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            var candidate = Path.Combine(
                directory.FullName, "desktop", "CraftPilot.AgentSetup", "wwwroot", name);
            if (File.Exists(candidate))
            {
                return File.ReadAllText(candidate);
            }

            directory = directory.Parent;
        }

        throw new FileNotFoundException(name);
    }
}
