using System.Drawing;
using System.Windows.Forms;

namespace CraftPilot.AgentSetup;

public sealed class TrayContext : ApplicationContext
{
    private readonly IHostApplicationLifetime _lifetime;
    private readonly AgentSetupService _service;
    private readonly UpdateChecker _updateChecker;
    private readonly PanelNotifier _notifier = new();
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
        UpdateChecker updateChecker,
        bool showPanel)
    {
        _panelUrl = panelUrl;
        _lifetime = lifetime;
        _service = service;
        _updateChecker = updateChecker;
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

        var restartItem = new ToolStripMenuItem("Starta om agenten");
        restartItem.Click += (_, _) => _ = RestartAgentAsync();

        var logItem = new ToolStripMenuItem("Öppna loggen");
        logItem.Click += (_, _) => OpenPanel("log");

        var exitItem = new ToolStripMenuItem("Avsluta");
        exitItem.Click += (_, _) => _lifetime.StopApplication();

        _menu = new ContextMenuStrip();
        _menu.Items.Add(openItem);
        _menu.Items.Add(_statusItem);
        _menu.Items.Add(new ToolStripSeparator());
        _menu.Items.Add(refreshItem);
        _menu.Items.Add(restartItem);
        _menu.Items.Add(logItem);
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
        _ = EnsureAgentStartedAsync();

        if (showPanel)
        {
            OpenPanel();
        }
    }

    private void OpenPanel(string? tab = null)
    {
        if (_exiting)
        {
            return;
        }

        if (_panel is null || _panel.IsDisposed)
        {
            _panel = new PanelWindow(_panelUrl, tab);
            _panel.FormClosed += (_, _) => _panel = null;
        }
        else if (tab is not null)
        {
            _panel.NavigateTo(tab);
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
            UpdateCheckResponse? version = null;
            try
            {
                version = await _updateChecker.CheckAsync(false);
            }
            catch
            {
                // Versionskontrollen är ett komplement; statusen gäller ändå.
            }

            var alerts = _notifier.Observe(status, version, DateTimeOffset.UtcNow);
            Post(() =>
            {
                if (!_exiting)
                {
                    Apply(TrayStatusClassifier.From(status));
                    _startupItem.Checked = status.StartupEnabled;
                    foreach (var alert in alerts)
                    {
                        ShowAlert(alert);
                    }
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

    /// <summary>
    /// Vid panelstart: vänta in Docker, starta agentcontainern om den saknas
    /// och larma om den inte kommer upp. En container som finns men är stoppad
    /// rörs inte — den har någon stoppat med flit.
    /// </summary>
    private async Task EnsureAgentStartedAsync()
    {
        try
        {
            var watchdog = new StartupAgentWatchdog(
                _service.GetStatusAsync,
                _service.StartAsync,
                stoppedByUser: () => _service.AgentStoppedByUser);
            var result = await watchdog.RunAsync(_lifetime.ApplicationStopping);
            var notice = StartupAgentNoticeFactory.For(result);
            if (notice is null)
            {
                return;
            }

            Post(() =>
            {
                if (!_exiting)
                {
                    _notifyIcon.ShowBalloonTip(
                        8000,
                        notice.Title,
                        notice.Message,
                        notice.Error ? ToolTipIcon.Error : ToolTipIcon.Info);
                }
            });

            // Statusen ritas om direkt så ikonen och panelen visar den startade
            // agenten utan att vänta på nästa poll.
            await PollAsync();
        }
        catch (OperationCanceledException)
        {
            // Panelen stängs; vakten behövs inte längre.
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Uppstartsvakten kunde inte köra: {ex.Message}");
        }
    }

    private void ShowAlert(PanelAlert alert) =>
        _notifyIcon.ShowBalloonTip(
            6000,
            alert.Title,
            alert.Message,
            alert.Kind == PanelAlertKind.Error ? ToolTipIcon.Error : ToolTipIcon.Warning);

    /// <summary>Startar om agenten från trayen utan att öppna panelen.</summary>
    private async Task RestartAgentAsync()
    {
        var result = await _service.RestartAgentAsync();
        Post(() =>
        {
            if (!_exiting)
            {
                _notifyIcon.ShowBalloonTip(
                    5000,
                    result.Ok ? "Agenten startar om" : "Kunde inte starta om agenten",
                    result.Ok
                        ? "Containern startas om. Statusen uppdateras inom några sekunder."
                        : result.Error ?? "Okänt fel.",
                    result.Ok ? ToolTipIcon.Info : ToolTipIcon.Error);
            }
        });
        await PollAsync();
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
