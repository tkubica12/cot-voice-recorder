using System.Diagnostics;
using System.Windows;
using System.Windows.Threading;
using Microsoft.Win32;
using VoicePrompt.App.Services;
using VoicePrompt.App.Views;
using VoicePrompt.Core.Infrastructure;
using Application = System.Windows.Application;

namespace VoicePrompt.App;

/// <summary>
/// Application entry point. VoicePrompt is a <b>tray-first</b> app: it starts hidden, with no
/// window and no taskbar entry, and the notification-area icon is the primary surface.
/// A second launch simply asks the running instance to show its window.
/// </summary>
public partial class App : Application
{
    private readonly SignedOutFixture? _fixture;
    private DispatcherTimer? _fixtureTimer;
    private bool _fixtureFailure;
    private SingleInstanceGuard? _instance;
    private AppHost? _host;
    private TrayIconHost? _tray;
    private MainWindow? _window;
    private DictationController? _dictation;
    private bool _exiting;

    public App() { }
    internal App(SignedOutFixture fixture) => _fixture = fixture;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        _fixture?.TraceProgress("startup");

        var quitRequested = e.Args.Any(a =>
            a.Equals("--quit", StringComparison.OrdinalIgnoreCase)
            || a.Equals("/quit", StringComparison.OrdinalIgnoreCase));

        _instance = SingleInstanceGuard.Acquire(_fixture is null
            ? SingleInstanceGuard.DefaultName : SignedOutFixture.InstanceName(_fixture.Key));
        _fixture?.TraceProgress("instance-acquired");
        if (!_instance.IsPrimary)
        {
            if (_fixture is not null) throw new InvalidOperationException("Isolated fixture instance already exists.");
            // Another instance owns the tray icon. Either ask it to shut down (--quit, used by
            // the installer before an upgrade) or surface its window, then quit quietly.
            if (quitRequested)
            {
                _instance.SignalQuit();
                // Block until the tray app has actually exited so `VoicePrompt.exe --quit`
                // is a synchronous "stop the app" command the installer can wait on.
                _instance.WaitForPrimaryExit(TimeSpan.FromSeconds(20));
            }
            else
            {
                _instance.SignalExistingInstance();
            }

            _instance.Dispose();
            _instance = null;
            // Exit directly: the dispatcher loop has not started yet, so Shutdown() would not
            // be processed and the helper process would linger (the installer waits on it).
            Environment.Exit(0);
            return;
        }

        if (quitRequested)
        {
            // Nothing was running; honour the request by exiting immediately.
            _instance.Dispose();
            _instance = null;
            Environment.Exit(0);
            return;
        }

        DispatcherUnhandledException += OnDispatcherUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += (_, args) =>
            _host?.Log.Error($"fatal: {(args.ExceptionObject as Exception)?.GetType().Name ?? "unknown"}");

        _tray = new TrayIconHost(TrayIconHost.LoadAppIcon());
        _fixture?.TraceProgress("tray-created");
        _host = _fixture is null ? AppHost.Create(_tray)
            : AppHost.CreateSignedOutFixture(_fixture.Root, _tray, new SignedOutFixture.ForbiddenClipboard());
        if (_fixture is not null) SignedOutFixture.ValidateHost(_host);
        _fixture?.TraceProgress("host-created");

        _tray.SetPausedChecked(_host.Settings.NotificationsPaused);
        _tray.Open += () => Dispatcher.BeginInvoke(ShowMainWindow);
        _tray.CopyLatest += () => Dispatcher.BeginInvoke(CopyLatestAsync);
        _tray.PauseChanged += paused => Dispatcher.BeginInvoke(() => SetPaused(paused));
        _tray.ExitRequested += () => Dispatcher.BeginInvoke(ExitApplication);

        _instance.ListenForActivation(
            () => Dispatcher.BeginInvoke(ShowMainWindow),
            () => Dispatcher.BeginInvoke(ExitApplication));

        SystemEvents.PowerModeChanged += OnPowerModeChanged;
        SystemEvents.SessionEnding += OnSessionEnding;
        SystemEvents.SessionSwitch += OnSessionSwitch;
        _fixture?.TraceProgress("system-events-subscribed");

        _host.Realtime.StateChanged += state =>
            Dispatcher.BeginInvoke(() => _tray?.SetStatusText(state.ToString()));

        _host.History.Changed += () =>
            Dispatcher.BeginInvoke(() => _tray?.SetCopyLatestEnabled(_host.History.Latest() is not null));

        _tray.SetCopyLatestEnabled(_host.History.Latest() is not null);
        _tray.SetStatusText(_host.OAuthConfigured ? "Starting" : "Not configured");

        _host.Start();
        _fixture?.TraceProgress("host-started");
        _dictation = new DictationController(_host, _tray, Dispatcher);
        _fixture?.TraceProgress("dictation-created");
        try
        {
            _host.Recovery.Cleanup();
            if (_host.Recovery.List().Count > 0)
                _tray.Notify("Unfinished dictation", "Open VoicePrompt > Recovery to finish saved dictations. Nothing is pasted automatically.");
        }
        catch (Exception ex)
        {
            if (_fixture is not null) throw;
            _host.Log.Warn($"dictation: recovery discovery failed ({ex.GetType().Name})");
            _tray.Notify("Recovery needs attention", "Saved dictation data could not be read. Open Recovery for details.",
                Core.Notifications.NotificationKind.Warning);
        }
        try
        {
            _dictation.Configure();
            _fixture?.TraceProgress("dictation-configured");
        }
        catch (Exception ex)
        {
            if (_fixture is not null) throw;
            _host.Log.Warn($"dictation: shortcut registration failed ({ex.GetType().Name})");
            _tray.Notify("Dictation shortcut unavailable", "Choose another shortcut in Settings.",
                Core.Notifications.NotificationKind.Warning);
        }

        if (!_host.OAuthConfigured && _fixture is null)
        {
            _tray.Notify(
                "VoicePrompt",
                "Sign-in is not configured yet. Open the window for setup details.",
                Core.Notifications.NotificationKind.Warning);
        }
        if (_fixture is not null)
        {
            _fixtureTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(_fixture.Seconds) };
            _fixtureTimer.Tick += (_, _) => ExitApplication();
            _fixtureTimer.Start();
            Dispatcher.BeginInvoke(() =>
            {
                _fixture.TraceProgress("dispatcher-ready");
                _fixture.Publish(_host.History.All().Count);
            }, DispatcherPriority.ApplicationIdle);
        }
    }

    private void ShowMainWindow()
    {
        GetMainWindow()?.ShowWindow();
    }

    private MainWindow? GetMainWindow()
    {
        if (_host is null)
        {
            return null;
        }

        if (_window is null)
        {
            _window = new MainWindow(_host, _dictation!);
            _window.PauseStateChanged += paused => _tray?.SetPausedChecked(paused);
        }

        return _window;
    }

    internal MainWindow ShowSignedOutFixtureWindow()
    {
        if (_fixture is null)
            throw new InvalidOperationException("Only a marked signed-out fixture can open a nonactivating preview.");
        var window = GetMainWindow()
            ?? throw new InvalidOperationException("The signed-out fixture host is not initialized.");
        window.Title = "Isolated WPF reference client - no physical ports";
        window.ShowActivated = false;
        window.Show();
        return window;
    }

    private async void CopyLatestAsync()
    {
        if (_host is null)
        {
            return;
        }

        var result = await _host.Coordinator.CopyLatestAsync(CancellationToken.None);
        _tray?.Notify("VoicePrompt", result switch
        {
            Core.Clipboard.ClipboardCopyResult.Copied => "Latest transcript copied.",
            Core.Clipboard.ClipboardCopyResult.Failed => "The clipboard was busy; try again.",
            _ => "No transcript cached yet.",
        });
    }

    private void SetPaused(bool paused)
    {
        if (_host is null)
        {
            return;
        }

        _host.Settings.NotificationsPaused = paused;
        _host.SaveSettings();
        _window?.SetPaused(paused);
    }

    private void OnPowerModeChanged(object sender, PowerModeChangedEventArgs e)
    {
        switch (e.Mode)
        {
            case PowerModes.Suspend:
                Dispatcher.BeginInvoke(() => _dictation?.Interrupt());
                _host?.Realtime.Suspend();
                break;
            case PowerModes.Resume:
                _host?.Realtime.Resume();
                break;
        }
    }

    private void OnSessionEnding(object sender, SessionEndingEventArgs e) =>
        Dispatcher.BeginInvoke(ExitApplication);

    private void OnSessionSwitch(object sender, SessionSwitchEventArgs e)
    {
        if (e.Reason is SessionSwitchReason.SessionLock or SessionSwitchReason.SessionLogoff
            or SessionSwitchReason.RemoteDisconnect or SessionSwitchReason.ConsoleDisconnect)
            Dispatcher.BeginInvoke(() => _dictation?.Interrupt());
    }

    private void OnDispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        if (_fixture is not null)
        {
            Console.Error.WriteLine($"C# fixture UI failed: {e.Exception.GetType().Name}");
            _fixtureFailure = true;
            e.Handled = true;
            ExitApplication();
            return;
        }
        var site = new StackTrace(e.Exception, false).GetFrames()?
            .Select(frame =>
            {
                var method = frame.GetMethod();
                return $"{method?.DeclaringType?.FullName}.{method?.Name}";
            })
            .FirstOrDefault();
        _host?.Log.Error($"ui: unhandled {e.Exception.GetType().Name}"
                         + (site is null ? string.Empty : $" at {site}"));
        // A UI failure must not take the tray listener down.
        e.Handled = true;
    }

    private async void ExitApplication()
    {
        if (_exiting)
        {
            return;
        }

        _exiting = true;
        _fixtureTimer?.Stop();
        if (_dictation is not null)
        {
            if (_fixture is null) await _dictation.DisposeAsync();
            else
            {
                try { await _dictation.DisposeAsync(); }
                catch (Exception error) { Console.Error.WriteLine($"C# fixture dictation shutdown failed: {error.GetType().Name}"); _fixtureFailure = true; }
            }
        }
        if (_fixture is not null && _host is not null)
        {
            try { await _host.DisposeAsync(); }
            catch (Exception error) { Console.Error.WriteLine($"C# fixture shutdown failed: {error.GetType().Name}"); _fixtureFailure = true; }
            _host = null;
        }
        Shutdown(_fixtureFailure ? 1 : 0);
    }

    protected override void OnExit(ExitEventArgs e)
    {
        SystemEvents.PowerModeChanged -= OnPowerModeChanged;
        SystemEvents.SessionEnding -= OnSessionEnding;
        SystemEvents.SessionSwitch -= OnSessionSwitch;

        if (_host is not null)
        {
            try
            {
                _host.DisposeAsync().AsTask().Wait(TimeSpan.FromSeconds(5));
            }
            catch
            {
                // Shutdown must always complete.
            }
        }

        _tray?.Dispose();
        _instance?.Dispose();
        base.OnExit(e);
    }
}
