using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class PanelUpdaterTests
{
    private const string ReleaseJson = """
        {
          "tag_name": "v0.7.3",
          "html_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/tag/v0.7.3",
          "assets": [
            {
              "name": "CraftPilot.AgentSetup.exe",
              "browser_download_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/download/v0.7.3/CraftPilot.AgentSetup.exe"
            },
            {
              "name": "SHA256SUMS",
              "browser_download_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/download/v0.7.3/SHA256SUMS"
            },
            {
              "name": "craftpilot-agent-linux.tar.gz",
              "browser_download_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/download/v0.7.3/craftpilot-agent-linux.tar.gz"
            }
          ]
        }
        """;

    [Fact]
    public void TryParseRelease_reads_panel_exe_and_checksums()
    {
        var ok = PanelUpdater.TryParseRelease(ReleaseJson, out var release, out var error);

        Assert.True(ok, error);
        Assert.Equal("v0.7.3", release.Tag);
        Assert.EndsWith("/v0.7.3/CraftPilot.AgentSetup.exe", release.InstallerUrl);
        Assert.EndsWith("/v0.7.3/SHA256SUMS", release.ChecksumsUrl);
        Assert.Equal("https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/tag/v0.7.3", release.ReleaseUrl);
    }

    [Fact]
    public void TryParseRelease_rejects_assets_outside_the_repository()
    {
        const string json = """
            {
              "tag_name": "v0.7.3",
              "html_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/tag/v0.7.3",
              "assets": [
                {
                  "name": "CraftPilot.AgentSetup.exe",
                  "browser_download_url": "https://evil.example.com/CraftPilot.AgentSetup.exe"
                },
                {
                  "name": "SHA256SUMS",
                  "browser_download_url": "https://evil.example.com/SHA256SUMS"
                }
              ]
            }
            """;

        var ok = PanelUpdater.TryParseRelease(json, out _, out var error);

        Assert.False(ok);
        Assert.Contains("SHA256SUMS", error, StringComparison.Ordinal);
    }

    [Fact]
    public void TryParseRelease_rejects_release_without_panel_files()
    {
        const string json = """
            {
              "tag_name": "v0.7.3",
              "html_url": "https://github.com/cozilic/craftpilot-runtime-agent-rs/releases/tag/v0.7.3",
              "assets": []
            }
            """;

        Assert.False(PanelUpdater.TryParseRelease(json, out _, out _));
        Assert.False(PanelUpdater.TryParseRelease("inte json", out _, out _));
    }

    [Fact]
    public void ParseChecksum_finds_the_panel_hash()
    {
        const string hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        var sums = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  SHA256SUMS\r\n" +
            hash.ToUpperInvariant() + " *CraftPilot.AgentSetup.exe\r\n";

        Assert.Equal(hash, PanelUpdater.ParseChecksum(sums, "CraftPilot.AgentSetup.exe"));
    }

    [Fact]
    public void ParseChecksum_ignores_other_files_and_malformed_hashes()
    {
        Assert.Null(PanelUpdater.ParseChecksum(null, "CraftPilot.AgentSetup.exe"));
        Assert.Null(PanelUpdater.ParseChecksum("abc  CraftPilot.AgentSetup.exe\n", "CraftPilot.AgentSetup.exe"));
        Assert.Null(PanelUpdater.ParseChecksum(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  annan-fil.exe\n",
            "CraftPilot.AgentSetup.exe"));
    }
}
