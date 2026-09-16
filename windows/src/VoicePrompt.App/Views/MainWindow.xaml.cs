using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Globalization;
using System.Net.Http;
using System.Windows;
using System.Windows.Input;
using VoicePrompt.App.Services;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Realtime;
using VoicePrompt.Core.Dictation;
using KeyEventArgs = System.Windows.Input.KeyEventArgs;
using MessageBox = System.Windows.MessageBox;

namespace VoicePrompt.App.Views;

/// <summary>A cached transcript as shown in the history list (preview + metadata only).</summary>
public sealed class HistoryRow
{
    public required string TranscriptId { get; init; }
    public required string Preview { get; init; }
    public required string Meta { get; init; }

    /// <summary>Announced by screen readers for the list item.</summary>
    public override string ToString() => $"{Preview}. {Meta}";
}

/// <summary>
/// The compact settings + history window. It is created lazily, hidden rather than closed, and
/// is never required for the app to function — the tray is the primary surface.
/// </summary>
public partial class MainWindow : Window
{
    public static readonly RoutedCommand HideWindowCommand = new(nameof(HideWindowCommand), typeof(MainWindow));
    public static readonly RoutedCommand RefreshCommand = new(nameof(RefreshCommand), typeof(MainWindow));

    private readonly AppHost _host;
    private readonly DictationController _dictation;
    private readonly ObservableCollection<HistoryRow> _rows = new();

    public MainWindow(AppHost host, DictationController dictation)
    {
        _host = host;
        _dictation = dictation;
        InitializeComponent();

        HistoryList.ItemsSource = _rows;
        BackendUrlBox.Text = _host.Settings.BackendBaseUrl;
        AutoStartCheck.IsChecked = _host.Settings.AutoStart;
        PauseCheck.IsChecked = _host.Settings.NotificationsPaused;
        DictationEnabledCheck.IsChecked = _host.Settings.DictationEnabled;
        DictationShortcutBox.Text = _host.Settings.DictationShortcut;
        DictationToggleShortcutBox.Text = _host.Settings.DictationToggleShortcut;
        DictationLanguageBox.SelectedIndex = _host.Settings.DictationLanguage switch { "cs" => 1, "en" => 2, _ => 0 };
        AboutText.Text =
            $"VoicePrompt {AppHost.AppVersion} · .NET 8 · framework-dependent x64\n" +
            $"Local data: {_host.Paths.Root}\n" +
            "Transcripts are cached locally for 48 hours. Dictation audio is sent to MAI in Azure. "
            + "Recovery audio/text is encrypted with Windows DPAPI for this user, retained for 48 hours, and removed after successful completion or explicit discard.";

        CommandBindings.Add(new CommandBinding(HideWindowCommand, (_, _) => HideToTray()));
        CommandBindings.Add(new CommandBinding(RefreshCommand, (_, _) => _ = RefreshFromBackendAsync()));

        _host.Realtime.StateChanged += OnRealtimeStateChanged;
        _host.Auth.StatusChanged += OnAuthStatusChanged;
        _host.History.Changed += OnHistoryChanged;
        _dictation.RecoveryChanged += () => Dispatcher.BeginInvoke(() => _ = RefreshRecoveryAsync());
        _host.Coordinator.Handled += _ => Dispatcher.BeginInvoke(RefreshHistory);

        RefreshHistory();
        RenderRealtimeState(_host.Realtime.State);
        RenderAuthStatus(_host.Auth.Status);
        _ = RefreshRecoveryAsync();
    }

    /// <summary>Show (or re-show) the window and put focus somewhere useful.</summary>
    public void ShowWindow()
    {
        _ = RefreshRecoveryAsync();
        Show();
        if (WindowState == WindowState.Minimized)
        {
            WindowState = WindowState.Normal;
        }

        Activate();
        Focus();
        Tabs.Focus();
    }

    public void HideToTray() => Hide();

    /// <summary>Closing the window only hides it; Exit is an explicit tray action.</summary>
    protected override void OnClosing(CancelEventArgs e)
    {
        e.Cancel = true;
        HideToTray();
        base.OnClosing(e);
    }

    // ------------------------------------------------------------------ state rendering

    private void OnRealtimeStateChanged(RealtimeState state) =>
        Dispatcher.BeginInvoke(() => RenderRealtimeState(state));

    private void OnAuthStatusChanged(AuthStatus status) =>
        Dispatcher.BeginInvoke(() => RenderAuthStatus(status));

    private void OnHistoryChanged() => Dispatcher.BeginInvoke(RefreshHistory);

    private void RenderRealtimeState(RealtimeState state) => ConnectionText.Text = state switch
    {
        RealtimeState.Connected => "Connected",
        RealtimeState.Connecting => "Connecting…",
        RealtimeState.Reconnecting => "Reconnecting…",
        RealtimeState.Suspended => "Suspended",
        RealtimeState.SignInRequired => "Sign in required",
        RealtimeState.NotConfigured => "Not configured",
        _ => "Stopped",
    };

    private void RenderAuthStatus(AuthStatus status)
    {
        AuthText.Text = status.State switch
        {
            AuthState.SignedIn => status.Email ?? "Signed in",
            AuthState.SignInNeeded => "Sign in required",
            AuthState.NotConfigured => "Not configured",
            _ => "Signed out",
        };

        AuthDetailText.Text = status.State switch
        {
            AuthState.SignedIn =>
                $"Signed in as {status.Email}. The refresh token is encrypted with Windows DPAPI "
                + "(current user) and never leaves this machine.",
            AuthState.SignInNeeded =>
                "The saved refresh token was rejected (revoked, expired, or consent changed). "
                + "Sign in again to restore the connection.",
            AuthState.NotConfigured =>
                "No Google Desktop OAuth client is installed, so sign-in is disabled. "
                + "Place google-desktop-client.json next to the executable or in "
                + $"{_host.Paths.Root} and restart.",
            _ => "Not signed in. Sign-in opens your default browser and never shows an embedded web view.",
        };

        SignInButton.IsEnabled = _host.OAuthConfigured && status.State != AuthState.SignedIn;
        SignOutButton.IsEnabled = status.State is AuthState.SignedIn or AuthState.SignInNeeded;
    }

    private void RefreshHistory()
    {
        var selectedId = (HistoryList.SelectedItem as HistoryRow)?.TranscriptId;
        _rows.Clear();

        foreach (var entry in _host.History.All())
        {
            _rows.Add(ToRow(entry));
        }

        CacheText.Text = _rows.Count.ToString(CultureInfo.InvariantCulture);
        EmptyHistoryText.Visibility = _rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        CopySelectedButton.IsEnabled = _rows.Count > 0;
        CopyLatestButton.IsEnabled = _rows.Count > 0;
        ClearHistoryButton.IsEnabled = _rows.Count > 0;

        if (selectedId is not null)
        {
            HistoryList.SelectedItem = _rows.FirstOrDefault(r => r.TranscriptId == selectedId);
        }
    }

    private static HistoryRow ToRow(HistoryEntry entry)
    {
        var local = entry.CompletedAt.ToLocalTime();
        var expires = entry.CompletedAt + HistoryStore.Retention;
        var left = expires - DateTimeOffset.UtcNow;
        var remaining = left <= TimeSpan.Zero
            ? "expired"
            : left.TotalHours >= 1
                ? $"{(int)left.TotalHours} h left"
                : $"{Math.Max(1, (int)left.TotalMinutes)} min left";

        return new HistoryRow
        {
            TranscriptId = entry.TranscriptId,
            Preview = string.IsNullOrWhiteSpace(entry.Preview) ? "(no preview)" : entry.Preview,
            Meta = $"{local:g} · {entry.CharacterCount} chars · {remaining}",
        };
    }

    // ------------------------------------------------------------------ history actions

    private void OnHistoryDoubleClick(object sender, MouseButtonEventArgs e) => _ = CopySelectedAsync();

    private void OnHistoryKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key is Key.Enter or Key.Space)
        {
            e.Handled = true;
            _ = CopySelectedAsync();
        }
        else if (e.Key == Key.C && Keyboard.Modifiers == ModifierKeys.Control)
        {
            e.Handled = true;
            _ = CopySelectedAsync();
        }
    }

    private void OnCopySelected(object sender, RoutedEventArgs e) => _ = CopySelectedAsync();

    private async Task CopySelectedAsync()
    {
        if (HistoryList.SelectedItem is not HistoryRow row)
        {
            return;
        }

        var result = await _host.Coordinator.CopyAsync(row.TranscriptId, CancellationToken.None);
        ReportCopy(result);
    }

    private async void OnCopyLatest(object sender, RoutedEventArgs e)
    {
        var result = await _host.Coordinator.CopyLatestAsync(CancellationToken.None);
        ReportCopy(result);
    }

    private void ReportCopy(ClipboardCopyResult result) => BackendHealthText.Text = result switch
    {
        ClipboardCopyResult.Copied => "Copied to the clipboard.",
        ClipboardCopyResult.Failed => "The clipboard was busy; try again.",
        _ => "Nothing to copy.",
    };

    private void OnClearHistory(object sender, RoutedEventArgs e)
    {
        var confirm = MessageBox.Show(
            this,
            "Delete all locally cached transcripts? Transcripts still within the backend's "
            + "48-hour retention window can be fetched again with Refresh.",
            "Clear history",
            MessageBoxButton.OKCancel,
            MessageBoxImage.Warning,
            MessageBoxResult.Cancel);

        if (confirm == MessageBoxResult.OK)
        {
            _host.History.Clear();
            RefreshHistory();
        }
    }

    private void OnRefresh(object sender, RoutedEventArgs e) => _ = RefreshFromBackendAsync();

    private async Task RefreshFromBackendAsync()
    {
        RefreshButton.IsEnabled = false;
        try
        {
            var page = await _host.Api.ListTranscriptsAsync(null, 50, CancellationToken.None);
            var added = 0;

            foreach (var summary in page.Items)
            {
                if (_host.History.Get(summary.TranscriptId) is not null)
                {
                    continue;
                }

                var full = await _host.Api.GetTranscriptAsync(summary.TranscriptId, CancellationToken.None);
                _host.History.Add(new HistoryEntry
                {
                    TranscriptId = full.TranscriptId,
                    RecordingId = full.RecordingId,
                    Preview = full.Preview,
                    Body = full.Body,
                    CompletedAt = full.CompletedAt,
                    CachedAt = _host.Clock.UtcNow,
                    CharacterCount = full.CharacterCount,
                });
                added++;
            }

            BackendHealthText.Text = added == 0
                ? "History is up to date."
                : $"Fetched {added} transcript(s) from the backend.";
            RefreshHistory();
        }
        catch (ApiException ex)
        {
            BackendHealthText.Text = ex.Kind switch
            {
                ApiErrorKind.Unauthorized => "Sign in required to refresh history.",
                ApiErrorKind.Forbidden => "This account is not allowed to use the backend.",
                _ => $"Refresh failed ({ex.StatusCode?.ToString(CultureInfo.InvariantCulture) ?? "network"}).",
            };
        }
        catch (Exception)
        {
            BackendHealthText.Text = "Refresh failed (network).";
        }
        finally
        {
            RefreshButton.IsEnabled = true;
        }
    }

    // ------------------------------------------------------------------ settings actions

    private void OnApplyUrl(object sender, RoutedEventArgs e)
    {
        if (_dictation.IsBusy)
        {
            BackendHealthText.Text = "Stop dictation before changing the backend.";
            return;
        }
        _host.UpdateBackendUrl(BackendUrlBox.Text);
        BackendUrlBox.Text = _host.Settings.BackendBaseUrl;
        BackendHealthText.Text = "Backend URL saved.";
    }

    private async void OnCheckHealth(object sender, RoutedEventArgs e)
    {
        CheckHealthButton.IsEnabled = false;
        BackendHealthText.Text = "Checking…";
        try
        {
            using var probe = new HttpClient { Timeout = TimeSpan.FromSeconds(30) };
            using var response = await probe.GetAsync(
                _host.Settings.BackendBaseUrl.TrimEnd('/') + "/health/ready",
                CancellationToken.None);
            BackendHealthText.Text = response.IsSuccessStatusCode
                ? $"Backend reachable (HTTP {(int)response.StatusCode})."
                : $"Backend responded HTTP {(int)response.StatusCode}.";
        }
        catch (Exception)
        {
            BackendHealthText.Text = "Backend unreachable.";
        }
        finally
        {
            CheckHealthButton.IsEnabled = true;
        }
    }

    private async void OnSignIn(object sender, RoutedEventArgs e)
    {
        SignInButton.IsEnabled = false;
        AuthDetailText.Text = "Waiting for the browser to complete sign-in…";
        try
        {
            await _dictation.InterruptAsync();
            using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(5));
            var ok = await _host.Auth.SignInAsync(timeout.Token);
            if (ok)
            {
                _host.Realtime.Start();
                _host.Realtime.Reconnect();
            }
            else
            {
                AuthDetailText.Text = "Sign-in did not complete. Please try again.";
            }
        }
        catch (AuthCallbackException ex)
        {
            AuthDetailText.Text = ex.Message;
        }
        catch (OperationCanceledException)
        {
            AuthDetailText.Text = "Sign-in timed out.";
        }
        catch (Exception)
        {
            AuthDetailText.Text = "Sign-in failed. Check your network connection and try again.";
        }
        finally
        {
            RenderAuthStatus(_host.Auth.Status);
        }
    }

    private async void OnSignOut(object sender, RoutedEventArgs e)
    {
        await _dictation.InterruptAsync();
        _host.Auth.SignOut();
        RenderAuthStatus(_host.Auth.Status);
        _host.Realtime.Reconnect();
    }

    private void OnAutoStartChanged(object sender, RoutedEventArgs e)
    {
        var enabled = AutoStartCheck.IsChecked == true;
        if (!_host.AutoStart.Set(enabled))
        {
            AutoStartCheck.IsChecked = _host.AutoStart.IsEnabled;
            return;
        }

        _host.Settings.AutoStart = enabled;
        _host.SaveSettings();
    }

    private void OnApplyDictation(object sender, RoutedEventArgs e)
    {
        if (_dictation.IsBusy)
        {
            DictationStatusText.Text = "Stop dictation before changing its settings.";
            return;
        }
        var before = _host.Settings.Clone();
        try
        {
            _host.Settings.DictationEnabled = DictationEnabledCheck.IsChecked == true;
            _host.Settings.DictationShortcut = DictationShortcutBox.Text.Trim();
            _host.Settings.DictationToggleShortcut = DictationToggleShortcutBox.Text.Trim();
            _host.Settings.DictationLanguage = DictationLanguageBox.SelectedIndex switch { 1 => "cs", 2 => "en", _ => "auto" };
            _dictation.Configure();
            _host.SaveSettings();
            DictationStatusText.Text = _host.Settings.DictationEnabled
                ? $"Hold {_host.Settings.DictationShortcut}, or start/stop with {_host.Settings.DictationToggleShortcut}. Focus the destination before stopping hands-free dictation. Esc discards."
                : "Dictation disabled.";
        }
        catch (Exception ex)
        {
            _host.Settings.DictationEnabled = before.DictationEnabled;
            _host.Settings.DictationShortcut = before.DictationShortcut;
            _host.Settings.DictationToggleShortcut = before.DictationToggleShortcut;
            _host.Settings.DictationLanguage = before.DictationLanguage;
            _host.Log.Warn($"dictation: settings failed ({ex.GetType().Name})");
            DictationStatusText.Text = "Could not save dictation settings. Use an available shortcut including Ctrl or Alt.";
            try { _dictation.Configure(); }
            catch (Exception restoreError)
            {
                _host.Log.Warn($"dictation: shortcut restore failed ({restoreError.GetType().Name})");
                DictationStatusText.Text = "Shortcut unavailable. Choose another shortcut and Apply.";
            }
        }
    }

    private void OnPauseChanged(object sender, RoutedEventArgs e)
    {
        _host.Settings.NotificationsPaused = PauseCheck.IsChecked == true;
        _host.SaveSettings();
        PauseStateChanged?.Invoke(_host.Settings.NotificationsPaused);
    }

    private void OnRefreshRecovery(object sender, RoutedEventArgs e) => _ = RefreshRecoveryAsync();

    private async Task RefreshRecoveryAsync()
    {
        try
        {
            var selected = (RecoveryList.SelectedItem as RecoveryInfo)?.Id;
            var items = await Task.Run(() => _host.Recovery.List());
            RecoveryList.ItemsSource = items;
            RecoveryList.SelectedItem = items.FirstOrDefault(i => i.Id == selected);
            if (items.Count == 0) RecoveryStatusText.Text = "No unfinished dictations.";
        }
        catch (Exception ex)
        {
            _host.Log.Warn($"dictation: recovery listing failed ({ex.GetType().Name})");
            RecoveryStatusText.Text = "Could not read recovery storage. Check disk access.";
        }
    }

    private async void OnRecover(object sender, RoutedEventArgs e)
    {
        if (_dictation.IsBusy || RecoveryList.SelectedItem is not RecoveryInfo item)
        {
            RecoveryStatusText.Text = "Stop dictation and select a saved session first.";
            return;
        }
        RecoverButton.IsEnabled = false;
        RecoveryStatusText.Text = "Recovering saved audio; nothing will be pasted. Esc stops recovery and keeps saved data.";
        var progressTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromMilliseconds(500) };
        progressTimer.Tick += (_, _) =>
        {
            if (_dictation.Progress is { } progress)
                RecoveryStatusText.Text = $"Recovering: {progress.PendingChunks} chunks pending. "
                    + (progress.ServiceError is null ? "Esc stops and keeps saved data." : "Cloud unavailable; saved data is retained.");
        };
        progressTimer.Start();
        try
        {
            await _dictation.RecoverAsync(item.Id);
            await RefreshRecoveryAsync();
            RecoveryStatusText.Text = "Recovery complete. Open History and Copy the recovered text.";
        }
        catch (OperationCanceledException)
        {
            RecoveryStatusText.Text = "Recovery stopped. Saved data is retained for another attempt.";
        }
        catch (Exception ex)
        {
            _host.Log.Warn($"dictation: recovery failed ({ex.GetType().Name})");
            RecoveryStatusText.Text = ex is System.IO.InvalidDataException or System.Security.Cryptography.CryptographicException
                ? "Recovery data is unreadable or corrupt. Nothing was deleted; it can be explicitly discarded."
                : "Recovery incomplete; saved data is retained. Check the original account/backend, network and disk access, then retry.";
        }
        finally
        {
            progressTimer.Stop();
            RecoverButton.IsEnabled = true;
        }
    }

    private async void OnDiscardRecovery(object sender, RoutedEventArgs e)
    {
        if (_dictation.IsBusy || RecoveryList.SelectedItem is not RecoveryInfo item)
        {
            RecoveryStatusText.Text = "Stop dictation and select a saved session first.";
            return;
        }
        if (MessageBox.Show(this, "Permanently discard this saved dictation audio and recovery text?",
            "Discard recovery", MessageBoxButton.OKCancel, MessageBoxImage.Warning, MessageBoxResult.Cancel)
            != MessageBoxResult.OK) return;
        try
        {
            await Task.Run(() => _host.Recovery.Delete(item.Id));
            await RefreshRecoveryAsync();
        }
        catch (Exception ex)
        {
            _host.Log.Warn($"dictation: discard failed ({ex.GetType().Name})");
            RecoveryStatusText.Text = "Could not delete saved recovery data. Check disk access and retry.";
        }
    }

    /// <summary>Raised so the tray menu's checkbox can stay in sync with the window.</summary>
    public event Action<bool>? PauseStateChanged;

    /// <summary>Reflect a pause toggle made from the tray menu.</summary>
    public void SetPaused(bool paused) => PauseCheck.IsChecked = paused;
}
