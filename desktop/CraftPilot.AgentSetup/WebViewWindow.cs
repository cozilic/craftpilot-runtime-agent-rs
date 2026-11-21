using System.Diagnostics;
using System.Windows.Forms;

using Microsoft.Web.WebView2.Core;

namespace CraftPilot.AgentSetup;

public static class WebViewWindow
{
    private static readonly TimeSpan StartupTimeout = TimeSpan.FromSeconds(30);

    public static bool TryStart(
        string url,
        IHostApplicationLifetime lifetime,
        AgentSetupService service,
        UpdateChecker updateChecker,
        bool showPanel)
    {
        try
        {
            _ = CoreWebView2Environment.GetAvailableBrowserVersionString();
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"WebView2 Runtime är inte tillgänglig: {ex.Message}");
            return false;
        }

        var ready = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => Run(url, lifetime, service, updateChecker, showPanel, ready))
        {
            IsBackground = true,
            Name = "CraftPilot tray",
        };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();

        if (!ready.Task.Wait(StartupTimeout))
        {
            Console.Error.WriteLine("Panelen tog för lång tid att starta.");
            return false;
        }

        return ready.Task.Result;
    }

    public static void ShowFallbackMessage()
    {
        var thread = new Thread(() =>
        {
            try
            {
                MessageBox.Show(
                    "WebView2 Runtime saknas eller kunde inte startas.\n\nPanelen öppnas i din vanliga webbläsare.",
                    "CraftPilot Runtime Agent",
                    MessageBoxButtons.OK,
                    MessageBoxIcon.Information);
            }
            catch
            {
                // The browser fallback is still useful even if the message cannot be shown.
            }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
    }

    private static void Run(
        string url,
        IHostApplicationLifetime lifetime,
        AgentSetupService service,
        UpdateChecker updateChecker,
        bool showPanel,
        TaskCompletionSource<bool> ready)
    {
        try
        {
            Application.SetHighDpiMode(HighDpiMode.PerMonitorV2);
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            using var context = new TrayContext(url, lifetime, service, updateChecker, showPanel);
            ready.TrySetResult(true);
            Application.Run(context);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Kunde inte starta panelen: {ex.Message}");
            ready.TrySetResult(false);
        }
    }
}
