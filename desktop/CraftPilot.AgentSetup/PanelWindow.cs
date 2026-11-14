using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;

using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;

namespace CraftPilot.AgentSetup;

public sealed class PanelWindow : Form
{
    private readonly WebView2 _webView = new() { Dock = DockStyle.Fill };
    private readonly Icon _brandIcon = TrayIconFactory.CreateBrandIcon(64);
    private readonly string _panelUrl;
    private bool _initialized;
    private bool _allowClose;

    public PanelWindow(string panelUrl)
    {
        _panelUrl = panelUrl;

        Text = "CraftPilot Runtime Agent";
        Icon = _brandIcon;
        ClientSize = new Size(1180, 820);
        MinimumSize = new Size(820, 600);
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(8, 13, 17);

        Controls.Add(_webView);
        Shown += async (_, _) => await InitializeAsync();
        FormClosing += OnFormClosing;
    }

    public void AllowClose() => _allowClose = true;

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            _brandIcon.Dispose();
        }

        base.Dispose(disposing);
    }

    private void OnFormClosing(object? sender, FormClosingEventArgs e)
    {
        if (_allowClose || e.CloseReason is CloseReason.WindowsShutDown or CloseReason.TaskManagerClosing)
        {
            return;
        }

        e.Cancel = true;
        Hide();
    }

    private async Task InitializeAsync()
    {
        if (_initialized)
        {
            return;
        }

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
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Kunde inte initiera WebView2: {ex.Message}");
            MessageBox.Show(
                "Panelen kunde inte startas i WebView2. Kontrollera att Microsoft Edge WebView2 Runtime är installerat.",
                "CraftPilot Runtime Agent",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
            _allowClose = true;
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
