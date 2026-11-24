using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

// Panelen ska kunna starta ett installerat men stoppat Docker Desktop, och
// känna igen installationsfilen på samma platser som tidigare.
public sealed class DockerDesktopLaunchTests
{
    private const string ProgramFiles = @"C:\Program Files";
    private const string LocalAppData = @"C:\Users\test\AppData\Local";

    [Fact]
    public void Finds_Docker_Desktop_in_Program_Files_first()
    {
        var expected = Path.Combine(ProgramFiles, "Docker", "Docker", "Docker Desktop.exe");

        var found = DockerDesktopLocator.Find(
            path => path == expected || path.Contains("Local", StringComparison.Ordinal),
            ProgramFiles,
            LocalAppData);

        Assert.Equal(expected, found);
    }

    [Fact]
    public void Falls_back_to_the_per_user_install_directory()
    {
        var expected = Path.Combine(LocalAppData, "Programs", "Docker", "Docker Desktop.exe");

        var found = DockerDesktopLocator.Find(path => path == expected, ProgramFiles, LocalAppData);

        Assert.Equal(expected, found);
    }

    [Fact]
    public void Returns_null_when_Docker_Desktop_is_not_installed()
    {
        Assert.Null(DockerDesktopLocator.Find(_ => false, ProgramFiles, LocalAppData));
    }

    [Fact]
    public void Starts_Docker_Desktop_when_it_is_installed()
    {
        var started = new List<string>();
        var path = Path.Combine(ProgramFiles, "Docker", "Docker", "Docker Desktop.exe");

        var ok = DockerDesktopLauncher.Start(path, p =>
        {
            started.Add(p);
            return true;
        });

        Assert.True(ok);
        Assert.Equal(new[] { path }, started);
    }

    [Fact]
    public void Reports_failure_when_the_file_is_missing_or_will_not_start()
    {
        Assert.False(DockerDesktopLauncher.Start(null, _ => true));
        Assert.False(DockerDesktopLauncher.Start(@"C:\Docker Desktop.exe", _ => false));
    }
}
