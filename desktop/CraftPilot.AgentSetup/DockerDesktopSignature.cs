using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;

namespace CraftPilot.AgentSetup;

/// <summary>
/// Kontrollerar att en nedladdad exe är Authenticode-signerad av Docker innan
/// panelen startar den. Filen hämtas över HTTPS, men det som binder filen till
/// utgivaren är signaturen – därför verifieras den separat.
/// </summary>
public static class DockerDesktopSignature
{
    /// <summary>Utgivaren som måste finnas i signeringscertifikatets subject.</summary>
    public const string ExpectedPublisher = "Docker";

    private const uint WtdUiNone = 2;
    private const uint WtdRevokeNone = 0;
    private const uint WtdChoiceFile = 1;
    private const uint WtdStateActionVerify = 1;
    private const uint WtdStateActionClose = 2;
    private const uint WtdRevocationCheckNone = 0x10;

    private static readonly Guid ActionGenericVerifyV2 = new("00AAC56B-CD44-11d0-8CC2-00C04FC295EE");

    public static bool IsTrusted(string? path)
    {
        if (!OperatingSystem.IsWindows() || string.IsNullOrWhiteSpace(path) || !File.Exists(path))
        {
            return false;
        }

        return ChainIsTrusted(path) && PublisherIsDocker(path);
    }

    private static bool ChainIsTrusted(string path)
    {
        var filePath = Marshal.StringToHGlobalUni(path);
        var fileInfo = Marshal.AllocHGlobal(Marshal.SizeOf<WinTrustFileInfo>());
        var data = Marshal.AllocHGlobal(Marshal.SizeOf<WinTrustData>());

        try
        {
            Marshal.StructureToPtr(new WinTrustFileInfo
            {
                cbStruct = (uint)Marshal.SizeOf<WinTrustFileInfo>(),
                pcwszFilePath = filePath,
            }, fileInfo, false);

            Marshal.StructureToPtr(new WinTrustData
            {
                cbStruct = (uint)Marshal.SizeOf<WinTrustData>(),
                dwUIChoice = WtdUiNone,
                fdwRevocationChecks = WtdRevokeNone,
                dwUnionChoice = WtdChoiceFile,
                pFile = fileInfo,
                dwStateAction = WtdStateActionVerify,
                dwProvFlags = WtdRevocationCheckNone,
            }, data, false);

            var result = WinVerifyTrust(IntPtr.Zero, ActionGenericVerifyV2, data);

            // Stäng state-handtaget även när kontrollen gick igenom.
            var state = Marshal.PtrToStructure<WinTrustData>(data);
            state.dwStateAction = WtdStateActionClose;
            Marshal.StructureToPtr(state, data, false);
            WinVerifyTrust(IntPtr.Zero, ActionGenericVerifyV2, data);

            return result == 0;
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException or CryptographicException)
        {
            // Utan wintrust.dll kan panelen inte avgöra något – behandla filen som overifierad.
            return false;
        }
        finally
        {
            Marshal.FreeHGlobal(data);
            Marshal.FreeHGlobal(fileInfo);
            Marshal.FreeHGlobal(filePath);
        }
    }

    private static bool PublisherIsDocker(string path)
    {
        try
        {
            using var certificate = new X509Certificate2(X509Certificate.CreateFromSignedFile(path));
            return certificate.Subject.Contains(ExpectedPublisher, StringComparison.OrdinalIgnoreCase);
        }
        catch (CryptographicException)
        {
            return false;
        }
    }

    [DllImport("wintrust.dll", ExactSpelling = true, SetLastError = false)]
    private static extern int WinVerifyTrust(
        IntPtr hwnd,
        [MarshalAs(UnmanagedType.LPStruct)] Guid actionId,
        IntPtr data);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WinTrustFileInfo
    {
        public uint cbStruct;
        public IntPtr pcwszFilePath;
        public IntPtr hFile;
        public IntPtr pgKnownSubject;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinTrustData
    {
        public uint cbStruct;
        public IntPtr pPolicyCallbackData;
        public IntPtr pSIPClientData;
        public uint dwUIChoice;
        public uint fdwRevocationChecks;
        public uint dwUnionChoice;
        public IntPtr pFile;
        public uint dwStateAction;
        public IntPtr hWVTStateData;
        public IntPtr pwszURLReference;
        public uint dwProvFlags;
        public uint dwUIContext;
        public IntPtr pSignatureSettings;
    }
}
