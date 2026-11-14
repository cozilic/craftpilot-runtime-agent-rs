using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;

using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;

namespace CraftPilot.AgentSetup;

public static class WebViewWindow
{
    private static readonly TimeSpan StartupTimeout = TimeSpan.FromSeconds(30);

    public static bool TryShow(string url, IHostApplicationLifetime lifetime)
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
        var thread = new Thread(() => Run(url, lifetime, ready))
        {
            IsBackground = true,
            Name = "CraftPilot panel",
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
        TaskCompletionSource<bool> ready)
    {
        try
        {
            Application.SetHighDpiMode(HighDpiMode.PerMonitorV2);
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            using var form = new PanelForm(url, lifetime, ready);
            Application.Run(form);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Kunde inte starta panelen: {ex.Message}");
            ready.TrySetResult(false);
        }
    }

    private sealed class PanelForm : Form
    {
        private readonly IHostApplicationLifetime _lifetime;
        private readonly TaskCompletionSource<bool> _ready;
        private readonly WebView2 _webView = new() { Dock = DockStyle.Fill };
        private readonly string _panelUrl;
        private bool _initialized;

        public PanelForm(
            string panelUrl,
            IHostApplicationLifetime lifetime,
            TaskCompletionSource<bool> ready)
        {
            _panelUrl = panelUrl;
            _lifetime = lifetime;
            _ready = ready;

            Text = "CraftPilot Runtime Agent";
            ClientSize = new Size(1180, 820);
            MinimumSize = new Size(820, 600);
            StartPosition = FormStartPosition.CenterScreen;
            BackColor = Color.FromArgb(8, 13, 17);

            Controls.Add(_webView);
            Shown += async (_, _) => await InitializeAsync();
            FormClosed += (_, _) =>
            {
                if (_initialized)
                {
                    _lifetime.StopApplication();
                }
            };
        }

        private async Task InitializeAsync()
        {
            try
            {
                var userDataFolder = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                    "CraftPilot",
                    "Agent",
                    "WebView2");
                Directory.CreateDirectory(userDataFolder);

                var environment = await CoreWebView2Environment.CreateAsync(
                    browserExecutableFolder: null,
                    userDataFolder: userDataFolder);
                await _webView.EnsureCoreWebView2Async(environment);

                var core = _webView.CoreWebView2;
                core.Settings.AreDevToolsEnabled = false;
                core.Settings.AreDefaultContextMenusEnabled = false;
                core.Settings.IsStatusBarEnabled = false;
                core.NewWindowRequested += OnNewWindowRequested;
                core.NavigationStarting += OnNavigationStarting;

                _webView.Source = new Uri(_panelUrl);
                _initialized = true;
                _ready.TrySetResult(true);
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Kunde inte initiera WebView2: {ex.Message}");
                _ready.TrySetResult(false);
                Close();
            }
        }

        private void OnNavigationStarting(object? sender, CoreWebView2NavigationStartingEventArgs e)
        {
            if (PanelNavigation.IsLocalPanelUri(e.Uri, _panelUrl))
            {
                return;
            }

            e.Cancel = true;
            OpenExternal(e.Uri);
        }

        private void OnNewWindowRequested(object? sender, CoreWebView2NewWindowRequestedEventArgs e)
        {
            e.Handled = true;
            OpenExternal(e.Uri);
        }

        private static void OpenExternal(string? value)
        {
            if (!PanelNavigation.IsSafeExternalUri(value))
            {
                return;
            }

            try
            {
                Process.Start(new ProcessStartInfo(value!) { UseShellExecute = true });
            }
            catch
            {
                // The link is optional; a failed browser launch must not close the panel.
            }
        }
    }
}
