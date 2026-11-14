using System.Drawing;
using System.Windows.Forms;

namespace CraftPilot.AgentSetup;

public sealed class TrayContext : ApplicationContext
{
    private readonly IHostApplicationLifetime _lifetime;
    private readonly AgentSetupService _service;
    private readonly string _panelUrl;
    private readonly NotifyIcon _notifyIcon;
    private readonly ContextMenuStrip _menu;
    private readonly ToolStripMenuItem _statusItem;
    private readonly ToolStripMenuItem _startupItem;
    private readonly System.Windows.Forms.Timer _timer;
    private readonly Control _dispatcher = new();
    private readonly Dictionary<TrayState, Icon> _icons;
    private PanelWindow? _panel;
    private TrayState _state = TrayState.Busy;
    private bool _polling;
    private bool _exiting;
    private bool _disposed;

    public TrayContext(
        string panelUrl,
        IHostApplicationLifetime lifetime,
        AgentSetupService service,
        bool showPanel)
    {
        _panelUrl = panelUrl;
        _lifetime = lifetime;
        _service = service;
        _dispatcher.CreateControl();

        _icons = new Dictionary<TrayState, Icon>
        {
            [TrayState.Busy] = TrayIconFactory.Create(Color.FromArgb(120, 183, 255)),
            [TrayState.Ok] = TrayIconFactory.Create(Color.FromArgb(70, 214, 154)),
            [TrayState.Warn] = TrayIconFactory.Create(Color.FromArgb(243, 185, 95)),
            [TrayState.Bad] = TrayIconFactory.Create(Color.FromArgb(255, 115, 115)),
        };

        _statusItem = new ToolStripMenuItem("Status: startar…") { Enabled = false };
        _startupItem = new ToolStripMenuItem("Starta med Windows")
        {
            CheckOnClick = true,
            Checked = StartupRegistration.IsEnabled,
        };
        _startupItem.Click += OnStartupClick;

        var openItem = new ToolStripMenuItem("Öppna panelen");
        openItem.Click += (_, _) => OpenPanel();

        var refreshItem = new ToolStripMenuItem("Kontrollera igen");
        refreshItem.Click += (_, _) => _ = PollAsync();

        var exitItem = new ToolStripMenuItem("Avsluta");
        exitItem.Click += (_, _) => _lifetime.StopApplication();

        _menu = new ContextMenuStrip();
        _menu.Items.Add(openItem);
        _menu.Items.Add(_statusItem);
        _menu.Items.Add(new ToolStripSeparator());
        _menu.Items.Add(refreshItem);
        _menu.Items.Add(_startupItem);
        _menu.Items.Add(new ToolStripSeparator());
        _menu.Items.Add(exitItem);

        _notifyIcon = new NotifyIcon
        {
            Icon = _icons[_state],
            Text = "CraftPilot: startar…",
            Visible = true,
            ContextMenuStrip = _menu,
        };
        _notifyIcon.DoubleClick += (_, _) => OpenPanel();
        _notifyIcon.MouseClick += (_, e) =>
        {
            if (e.Button == MouseButtons.Left)
            {
                OpenPanel();
            }
        };

        _timer = new System.Windows.Forms.Timer { Interval = 10_000 };
        _timer.Tick += (_, _) => _ = PollAsync();
        _timer.Start();

        _lifetime.ApplicationStopping.Register(OnApplicationStopping);
        _ = PollAsync();

        if (showPanel)
        {
            OpenPanel();
        }
    }

    private void OpenPanel()
    {
        if (_exiting)
        {
            return;
        }

        if (_panel is null || _panel.IsDisposed)
        {
            _panel = new PanelWindow(_panelUrl);
            _panel.FormClosed += (_, _) => _panel = null;
        }

        if (!_panel.Visible)
        {
            _panel.Show();
        }

        if (_panel.WindowState == FormWindowState.Minimized)
        {
            _panel.WindowState = FormWindowState.Normal;
        }

        _panel.Activate();
    }

    private void OnStartupClick(object? sender, EventArgs e)
    {
        try
        {
            StartupRegistration.SetEnabled(_startupItem.Checked);
        }
        catch (Exception ex)
        {
            _startupItem.Checked = !_startupItem.Checked;
            MessageBox.Show(
                $"Kunde inte ändra automatisk start: {ex.Message}",
                "CraftPilot Runtime Agent",
                MessageBoxButtons.OK,
                MessageBoxIcon.Warning);
        }
    }

    private async Task PollAsync()
    {
        if (_polling || _exiting)
        {
            return;
        }

        _polling = true;
        try
        {
            var status = await _service.GetStatusAsync();
            Post(() =>
            {
                if (!_exiting)
                {
                    Apply(TrayStatusClassifier.From(status));
                    _startupItem.Checked = status.StartupEnabled;
                }
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                if (!_exiting)
                {
                    Apply(new TrayStatus(TrayState.Bad, "Status kunde inte läsas", "CraftPilot: status kunde inte läsas"));
                }
            });
            Console.Error.WriteLine($"Kunde inte läsa panelstatus: {ex.Message}");
        }
        finally
        {
            _polling = false;
        }
    }

    private void Apply(TrayStatus status)
    {
        if (_state != status.State)
        {
            _notifyIcon.Icon = _icons[status.State];
            _state = status.State;
        }

        _notifyIcon.Text = status.Tooltip;
        _statusItem.Text = $"Status: {status.Label}";
    }

    private void OnApplicationStopping()
    {
        if (_dispatcher.IsDisposed)
        {
            return;
        }

        try
        {
            if (_dispatcher.InvokeRequired)
            {
                _dispatcher.BeginInvoke(new Action(CloseForExit));
            }
            else
            {
                CloseForExit();
            }
        }
        catch
        {
            // The tray may already be gone during a fast shutdown.
        }
    }

    private void Post(Action action)
    {
        try
        {
            if (!_dispatcher.IsDisposed)
            {
                _dispatcher.BeginInvoke(action);
            }
        }
        catch
        {
            // The dispatch handle can disappear while the app is shutting down.
        }
    }

    private void CloseForExit()
    {
        if (_exiting)
        {
            return;
        }

        _exiting = true;
        _timer.Stop();
        _notifyIcon.Visible = false;

        if (_panel is not null && !_panel.IsDisposed)
        {
            _panel.AllowClose();
            _panel.Close();
        }

        ExitThread();
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing && !_disposed)
        {
            _disposed = true;
            _timer.Dispose();
            _notifyIcon.Dispose();
            _menu.Dispose();
            _dispatcher.Dispose();
            foreach (var icon in _icons.Values)
            {
                icon.Dispose();
            }
            _icons.Clear();
        }

        base.Dispose(disposing);
    }
}
