using System.Net.Http;
using VoicePrompt.App.Services;
using VoicePrompt.Core;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;
using VoicePrompt.Core.Realtime;
using VoicePrompt.Core.Settings;
using VoicePrompt.Core.Dictation;

namespace VoicePrompt.App;

/// <summary>
/// Composition root: builds every service from the local state directory and the (optional)
/// Desktop OAuth client JSON, and owns the background lifecycle (realtime connection,
/// periodic history cleanup).
///
/// When no Desktop client JSON is present the host still starts fully — auth reports
/// <see cref="AuthState.NotConfigured"/>, the realtime loop stays idle, and the UI explains
/// what is missing. This keeps CI and first-run-before-configuration safe.
/// </summary>
public sealed class AppHost : IAsyncDisposable, IDictationHost
{
    public const string AppVersion = "1.4.5";

    private static readonly TimeSpan CleanupInterval = TimeSpan.FromMinutes(30);

    private readonly CancellationTokenSource _shutdown = new();
    private readonly HttpClient _apiHttp;
    private readonly HttpClient _tokenHttp;
    private HttpClient? _dictationHttp;
    private HttpClient? _refinementHttp;
    private readonly bool _firstRun;
    private Task? _cleanupLoop;

    private AppHost(
        AppPaths paths,
        ILog log,
        IClock clock,
        AppSettings settings,
        SettingsStore settingsStore,
        AuthManager auth,
        ApiClient api,
        HttpClient apiHttp,
        HttpClient tokenHttp,
        HistoryStore history,
        TranscriptCoordinator coordinator,
        RealtimeClient realtime,
        IAutoStartManager autoStart,
        bool oauthConfigured,
        bool firstRun)
    {
        Paths = paths;
        Log = log;
        Clock = clock;
        Settings = settings;
        SettingsStore = settingsStore;
        Auth = auth;
        Api = api;
        _apiHttp = apiHttp;
        _tokenHttp = tokenHttp;
        History = history;
        Coordinator = coordinator;
        Realtime = realtime;
        AutoStart = autoStart;
        OAuthConfigured = oauthConfigured;
        _firstRun = firstRun;
    }

    public AppPaths Paths { get; }
    public ILog Log { get; }
    public IClock Clock { get; }
    public AppSettings Settings { get; }
    public SettingsStore SettingsStore { get; }
    public AuthManager Auth { get; }
    public ApiClient Api { get; }
    public HistoryStore History { get; }
    public TranscriptCoordinator Coordinator { get; }
    public RealtimeClient Realtime { get; }
    public IAutoStartManager AutoStart { get; }
    public ApiClient DictationApi { get; private set; } = null!;
    public ApiClient DictationRefinementApi { get; private set; } = null!;
    public bool DictationBusy { get; set; }
    public RecoveryStore Recovery { get; private set; } = null!;
    public string RecoveryContext => Settings.BackendBaseUrl + "\n" + Auth.Status.Email;
    bool IDictationHost.CanDictate => Auth.Status.CanCallBackend;
    Task<string> IDictationHost.TranscribeAsync(byte[] wav, string language, CancellationToken ct) =>
        DictationApi.TranscribeDictationAsync(wav, language, ct);
    Task<DictationRefinement> IDictationHost.RefineAsync(string text, string previousText, CancellationToken ct) =>
        DictationRefinementApi.RefineDictationAsync(text, previousText, ct);

    /// <summary>False when no Desktop OAuth client JSON was found (safe unconfigured state).</summary>
    public bool OAuthConfigured { get; }

    public static AppHost Create(INotifier notifier, IClipboardWriter? clipboardWriter = null)
    {
        var clock = SystemClock.Instance;
        var fs = PhysicalFileSystem.Instance;
        var paths = AppPaths.Default();
        paths.EnsureCreated(fs);

        var log = new FileLog(paths.LogFile, clock);
        log.Info($"host: starting VoicePrompt {AppVersion}");

        var settingsStore = new SettingsStore(paths.SettingsFile, fs);
        var firstRun = !fs.FileExists(paths.SettingsFile);
        var settings = settingsStore.Load();

        var desktopConfig = DesktopClientLocator.TryLoad(AppContext.BaseDirectory, paths.Root, fs, log);

        var tokenHttp = new HttpClient { Timeout = TimeSpan.FromSeconds(30) };
        var protector = new DpapiSecretProtector();
        var tokenStore = new TokenStore(paths.TokensFile, protector, fs);
        var auth = new AuthManager(
            desktopConfig,
            new GoogleTokenClient(tokenHttp),
            tokenStore,
            new SystemBrowser(),
            LoopbackAuthListenerFactory.Instance,
            clock);

        var apiHttp = new HttpClient
        {
            BaseAddress = new Uri(AppSettings.NormalizeBaseUrl(settings.BackendBaseUrl) + "/"),
            Timeout = TimeSpan.FromSeconds(60),
        };
        apiHttp.DefaultRequestHeaders.UserAgent.ParseAdd($"VoicePrompt/{AppVersion}");

        var api = new ApiClient(apiHttp, new AuthBackendCredentials(auth),
            baseUrl: () => new Uri(settings.BackendBaseUrl + "/"));
        var history = new HistoryStore(paths.HistoryFile, fs, clock);
        var clipboard = new ClipboardCopier(clipboardWriter ?? new StaClipboardWriter());

        AppHost? host = null;
        var coordinator = new TranscriptCoordinator(
            api, history, clipboard, notifier, clock,
            notificationsPaused: () => settings.NotificationsPaused || host?.DictationBusy == true,
            log: log);

        var realtime = new RealtimeClient(
            new ApiRealtimeNegotiator(api, AppVersion),
            ClientWebSocketTransportFactory.Instance,
            clock,
            log);

        realtime.TranscriptCompleted += coordinator.HandleAsync;

        var autoStart = new RegistryAutoStartManager(
            Environment.ProcessPath ?? AppContext.BaseDirectory, log);

        host = new AppHost(
            paths, log, clock, settings, settingsStore, auth, api, apiHttp, tokenHttp,
            history, coordinator, realtime, autoStart, desktopConfig is not null, firstRun);
        host._dictationHttp = new HttpClient { Timeout = TimeSpan.FromSeconds(25) };
        host.Recovery = new RecoveryStore(paths.RecoveryDirectory, protector, clock);
        host.DictationApi = new ApiClient(host._dictationHttp, new AuthBackendCredentials(auth),
            new ApiRetryOptions { MaxRetries = 1 },
            baseUrl: () => new Uri(settings.BackendBaseUrl + "/"));
        host._refinementHttp = new HttpClient { Timeout = DictationPolisher.DefaultCallTimeout };
        host.DictationRefinementApi = new ApiClient(host._refinementHttp, new AuthBackendCredentials(auth),
            new ApiRetryOptions { MaxRetries = 0 },
            baseUrl: () => new Uri(settings.BackendBaseUrl + "/"));
        return host;
    }

    /// <summary>Start background work: history retention sweep and the realtime connection.</summary>
    public void Start()
    {
        History.Cleanup();
        _cleanupLoop ??= Task.Run(() => CleanupLoopAsync(_shutdown.Token));

        if (_firstRun)
        {
            // First run after install: the setup may already have registered auto-start
            // (the "Start automatically" task). Adopt the real state as the saved intent
            // instead of overwriting the user's installer choice with a default.
            Settings.AutoStart = AutoStart.IsEnabled;
            SaveSettings();
        }
        else if (Settings.AutoStart != AutoStart.IsEnabled)
        {
            // Reconcile the persisted intent with the registry (e.g. cleaned by another tool).
            AutoStart.Set(Settings.AutoStart);
        }

        if (OAuthConfigured)
        {
            Realtime.Start();
        }
        else
        {
            Log.Info("host: OAuth not configured; realtime idle");
        }
    }

    /// <summary>Persist the current settings snapshot.</summary>
    public void SaveSettings() => SettingsStore.Save(Settings);

    /// <summary>Apply a new backend base URL and force the realtime loop to renegotiate.</summary>
    public void UpdateBackendUrl(string url)
    {
        if (DictationBusy)
            throw new InvalidOperationException("Stop dictation before changing the backend.");
        var normalized = AppSettings.NormalizeBaseUrl(url);
        if (string.Equals(normalized, Settings.BackendBaseUrl, StringComparison.OrdinalIgnoreCase)
            && _apiHttp.BaseAddress is not null)
        {
            return;
        }

        Settings.BackendBaseUrl = normalized;
        SaveSettings();
        Log.Info("host: backend URL updated");
        Realtime.Reconnect();
    }

    private async Task CleanupLoopAsync(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            try
            {
                await Task.Delay(CleanupInterval, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                return;
            }

            try
            {
                var removed = History.Cleanup();
                if (!DictationBusy) Recovery.Cleanup();
                if (removed > 0)
                {
                    Log.Info($"cache: pruned {removed} expired transcript(s)");
                }
            }
            catch (Exception ex)
            {
                Log.Warn($"cache: cleanup failed ({ex.GetType().Name})");
            }
        }
    }

    public async ValueTask DisposeAsync()
    {
        _shutdown.Cancel();
        await Realtime.StopAsync().ConfigureAwait(false);

        if (_cleanupLoop is not null)
        {
            try
            {
                await _cleanupLoop.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // expected
            }
        }

        _apiHttp.Dispose();
        _dictationHttp?.Dispose();
        _refinementHttp?.Dispose();
        _tokenHttp.Dispose();
        _shutdown.Dispose();
        Log.Info("host: stopped");
    }
}
