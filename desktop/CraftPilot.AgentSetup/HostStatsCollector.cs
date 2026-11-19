using System.Net.NetworkInformation;
using System.Runtime.InteropServices;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Läser datorns resursläge för översikten: RAM, ledigt utrymme på
/// serverdata-disken och nätverkstrafik. Nätverket mäts som en differens
/// mellan två statusanrop, så första anropet visar 0 i stället för en
/// livstidssumma.
/// </summary>
public sealed class HostStatsCollector
{
    private readonly object _gate = new();
    private long _lastReceived;
    private long _lastSent;
    private long _lastSampleAt;

    public HostStats Collect(string? dataDir)
    {
        var (memoryUsed, memoryTotal) = ReadMemory();
        var (diskUsed, diskTotal) = ReadDisk(dataDir);
        var (down, up) = ReadNetworkRate();

        return new HostStats
        {
            MemoryUsedBytes = memoryUsed,
            MemoryTotalBytes = memoryTotal,
            DiskUsedBytes = diskUsed,
            DiskTotalBytes = diskTotal,
            NetworkDownBps = down,
            NetworkUpBps = up,
        };
    }

    private static (long Used, long Total) ReadMemory()
    {
        if (!OperatingSystem.IsWindows())
        {
            return (0, 0);
        }

        var status = new MemoryStatus { Length = (uint)Marshal.SizeOf<MemoryStatus>() };
        if (!GlobalMemoryStatusEx(ref status))
        {
            return (0, 0);
        }

        var total = (long)status.TotalPhysical;
        var available = (long)status.AvailablePhysical;
        return (Math.Max(0, total - available), total);
    }

    private static (long Used, long Total) ReadDisk(string? dataDir)
    {
        try
        {
            var path = string.IsNullOrWhiteSpace(dataDir) ? Environment.CurrentDirectory : dataDir;
            var root = Path.GetPathRoot(Path.GetFullPath(path));
            if (string.IsNullOrEmpty(root))
            {
                return (0, 0);
            }

            var drive = new DriveInfo(root);
            if (!drive.IsReady)
            {
                return (0, 0);
            }

            return (Math.Max(0, drive.TotalSize - drive.AvailableFreeSpace), drive.TotalSize);
        }
        catch (Exception ex) when (ex is ArgumentException or IOException or UnauthorizedAccessException)
        {
            return (0, 0);
        }
    }

    private (double Down, double Up) ReadNetworkRate()
    {
        long received = 0;
        long sent = 0;
        foreach (var network in NetworkInterface.GetAllNetworkInterfaces())
        {
            if (network.OperationalStatus != OperationalStatus.Up ||
                network.NetworkInterfaceType == NetworkInterfaceType.Loopback)
            {
                continue;
            }

            try
            {
                var statistics = network.GetIPv4Statistics();
                received += statistics.BytesReceived;
                sent += statistics.BytesSent;
            }
            catch (NetworkInformationException)
            {
                // En adapter kan försvinna mitt i mätningen.
            }
        }

        var now = Environment.TickCount64;
        lock (_gate)
        {
            var previousReceived = _lastReceived;
            var previousSent = _lastSent;
            var previousAt = _lastSampleAt;
            _lastReceived = received;
            _lastSent = sent;
            _lastSampleAt = now;

            if (previousAt == 0 || now <= previousAt)
            {
                return (0, 0);
            }

            var elapsed = (now - previousAt) / 1000.0;
            var down = Math.Max(0, received - previousReceived) / elapsed;
            var up = Math.Max(0, sent - previousSent) / elapsed;
            return (down, up);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MemoryStatus
    {
        public uint Length;
        public uint MemoryLoad;
        public ulong TotalPhysical;
        public ulong AvailablePhysical;
        public ulong TotalPageFile;
        public ulong AvailablePageFile;
        public ulong TotalVirtual;
        public ulong AvailableVirtual;
        public ulong AvailableExtendedVirtual;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GlobalMemoryStatusEx(ref MemoryStatus buffer);
}
