using System.Text.Json;
using System.Text.Json.Serialization;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Settings;

/// <summary>User-visible application settings. No secrets are ever stored here.</summary>
public sealed class AppSettings
{
    /// <summary>Deployed backend base URL (no trailing slash).</summary>
    [JsonPropertyName("backend_base_url")]
    public string BackendBaseUrl { get; set; } = DefaultBackendBaseUrl;

    /// <summary>When true, no toast is shown and transcripts are <b>not</b> auto-copied.</summary>
    [JsonPropertyName("notifications_paused")]
    public bool NotificationsPaused { get; set; }

    /// <summary>Whether the app should start automatically at user sign-in.</summary>
    [JsonPropertyName("auto_start")]
    public bool AutoStart { get; set; }

    [JsonPropertyName("dictation_enabled")]
    public bool DictationEnabled { get; set; } = true;

    [JsonPropertyName("dictation_shortcut")]
    public string DictationShortcut { get; set; } = "Ctrl+Alt+Space";

    [JsonPropertyName("dictation_toggle_shortcut")]
    public string DictationToggleShortcut { get; set; } = "Ctrl+Alt+Shift+Space";

    [JsonPropertyName("dictation_language")]
    public string DictationLanguage { get; set; } = "auto";

    [JsonPropertyName("dictation_refinement_enabled")]
    public bool DictationRefinementEnabled { get; set; }

    public const string DefaultBackendBaseUrl =
        "https://ca-api.ambitiousdesert-517ec9ed.swedencentral.azurecontainerapps.io";

    public AppSettings Clone() => new()
    {
        BackendBaseUrl = BackendBaseUrl,
        NotificationsPaused = NotificationsPaused,
        AutoStart = AutoStart,
        DictationEnabled = DictationEnabled,
        DictationShortcut = DictationShortcut,
        DictationToggleShortcut = DictationToggleShortcut,
        DictationLanguage = DictationLanguage,
        DictationRefinementEnabled = DictationRefinementEnabled,
    };

    /// <summary>Normalize and validate the backend URL, falling back to the default.</summary>
    public static string NormalizeBaseUrl(string? value)
    {
        var candidate = (value ?? string.Empty).Trim().TrimEnd('/');
        if (candidate.Length == 0)
        {
            return DefaultBackendBaseUrl;
        }

        return Uri.TryCreate(candidate, UriKind.Absolute, out var uri)
               && (uri.Scheme == Uri.UriSchemeHttps || uri.Scheme == Uri.UriSchemeHttp)
            ? candidate
            : DefaultBackendBaseUrl;
    }
}

/// <summary>Atomically persists <see cref="AppSettings"/> as JSON under LocalAppData.</summary>
public sealed class SettingsStore
{
    private static readonly JsonSerializerOptions Options = new() { WriteIndented = true };

    private readonly string _path;
    private readonly IFileSystem _fs;

    public SettingsStore(string path, IFileSystem fs)
    {
        _path = path;
        _fs = fs;
    }

    public AppSettings Load()
    {
        if (!_fs.FileExists(_path))
        {
            return new AppSettings();
        }

        try
        {
            var settings = JsonSerializer.Deserialize<AppSettings>(_fs.ReadAllText(_path), Options)
                           ?? new AppSettings();
            settings.BackendBaseUrl = AppSettings.NormalizeBaseUrl(settings.BackendBaseUrl);
            return settings;
        }
        catch
        {
            return new AppSettings();
        }
    }

    public void Save(AppSettings settings)
    {
        settings.BackendBaseUrl = AppSettings.NormalizeBaseUrl(settings.BackendBaseUrl);
        _fs.AtomicWrite(_path, JsonSerializer.Serialize(settings, Options));
    }
}
