using System.Text;

namespace CraftPilot.AgentSetup.Tests;

// Signaturen är det som binder den nedladdade filen till Docker. Testerna
// körs på Windows (där panelen byggs) och kräver ingen nätverksåtkomst.
public sealed class DockerDesktopSignatureTests
{
    [Fact]
    public void Rejects_a_missing_path()
    {
        Assert.False(DockerDesktopSignature.IsTrusted(null));
        Assert.False(DockerDesktopSignature.IsTrusted(string.Empty));
        Assert.False(DockerDesktopSignature.IsTrusted(
            Path.Combine(Path.GetTempPath(), $"craftpilot-missing-{Guid.NewGuid():N}.exe")));
    }

    [Fact]
    public void Rejects_an_unsigned_file()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }

        var path = Path.Combine(Path.GetTempPath(), $"craftpilot-unsigned-{Guid.NewGuid():N}.exe");
        File.WriteAllBytes(path, Encoding.ASCII.GetBytes("MZ"));
        try
        {
            Assert.False(DockerDesktopSignature.IsTrusted(path));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Rejects_a_signed_file_from_another_publisher()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }

        // Signerad av Microsoft, alltså inte Docker – utgivarkontrollen ska fälla den.
        var powershell = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.System),
            "WindowsPowerShell",
            "v1.0",
            "powershell.exe");

        if (!File.Exists(powershell))
        {
            return;
        }

        Assert.False(DockerDesktopSignature.IsTrusted(powershell));
    }
}
