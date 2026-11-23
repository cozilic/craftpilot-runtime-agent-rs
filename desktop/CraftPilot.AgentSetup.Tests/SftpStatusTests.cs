using System.Net;
using System.Net.Sockets;

using CraftPilot.AgentSetup;

namespace CraftPilot.AgentSetup.Tests;

public sealed class SftpStatusTests
{
    [Fact]
    public void ParsePort_accepts_only_valid_ports()
    {
        Assert.Equal(2223, SftpStatus.ParsePort("2223"));
        Assert.Equal(2223, SftpStatus.ParsePort(" 2223 "));
        Assert.Null(SftpStatus.ParsePort(null));
        Assert.Null(SftpStatus.ParsePort(string.Empty));
        Assert.Null(SftpStatus.ParsePort("0"));
        Assert.Null(SftpStatus.ParsePort("65536"));
        Assert.Null(SftpStatus.ParsePort("sftp"));
    }

    [Fact]
    public void IsEnabled_is_on_unless_it_is_explicitly_off()
    {
        Assert.True(SftpStatus.IsEnabled(null));
        Assert.True(SftpStatus.IsEnabled("true"));
        Assert.False(SftpStatus.IsEnabled("false"));
        Assert.False(SftpStatus.IsEnabled("FALSE"));
        Assert.False(SftpStatus.IsEnabled("0"));
        Assert.False(SftpStatus.IsEnabled("no"));
    }

    [Fact]
    public void BoundPortFrom_reads_the_published_host_port()
    {
        var json = "{\"2022/tcp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"2223\"},{\"HostIp\":\"::\",\"HostPort\":\"2223\"}]}";

        Assert.Equal(2223, SftpStatus.BoundPortFrom(json));
    }

    [Fact]
    public void BoundPortFrom_ignores_udp_and_empty_bindings()
    {
        Assert.Null(SftpStatus.BoundPortFrom("{\"2022/tcp\":[],\"2022/udp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"2223\"}]}"));
        Assert.Null(SftpStatus.BoundPortFrom("{\"2022/tcp\":null}"));
        Assert.Null(SftpStatus.BoundPortFrom("{}"));
    }

    [Fact]
    public void BoundPortFrom_survives_an_unexpected_docker_answer()
    {
        Assert.Null(SftpStatus.BoundPortFrom(null));
        Assert.Null(SftpStatus.BoundPortFrom("inte json"));
        Assert.Null(SftpStatus.BoundPortFrom("[]"));
    }

    [Fact]
    public void Sftp_is_enabled_unless_the_env_says_otherwise()
    {
        Assert.True(new SftpStatus().Enabled);
        Assert.False(new SftpStatus { Enabled = false }.Enabled);
    }

    [Fact]
    public void MovedFromPreferred_tells_when_the_agent_took_another_port()
    {
        Assert.True(new SftpStatus { PreferredPort = 2222, BoundPort = 2223 }.MovedFromPreferred);
        Assert.False(new SftpStatus { PreferredPort = 2223, BoundPort = 2223 }.MovedFromPreferred);
        Assert.False(new SftpStatus { PreferredPort = 2222 }.MovedFromPreferred);
        Assert.False(new SftpStatus { BoundPort = 2223 }.MovedFromPreferred);
    }

    [Fact]
    public async Task ProbeAsync_answers_when_something_listens_on_the_port()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();

        try
        {
            var port = ((IPEndPoint)listener.LocalEndpoint).Port;
            Assert.True(await SftpStatus.ProbeAsync(port, TimeSpan.FromSeconds(3)));
        }
        finally
        {
            listener.Stop();
        }
    }

    [Fact]
    public async Task ProbeAsync_reports_no_answer_on_a_closed_port()
    {
        // Porten frigörs direkt efter Stop(), så inget lyssnar när den provas.
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();

        Assert.False(await SftpStatus.ProbeAsync(port, TimeSpan.FromSeconds(3)));
    }
}
