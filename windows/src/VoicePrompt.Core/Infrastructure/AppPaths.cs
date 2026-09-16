namespace VoicePrompt.Core.Infrastructure;

/// <summary>
/// Per-user, roaming-free locations for all local state. Everything lives under
/// <c>%LOCALAPPDATA%\VoicePrompt</c> so an uninstall can leave user data intact and a
/// per-user (non-elevated) install never needs to write outside the profile.
/// </summary>
public sealed class AppPaths
{
    public const string ProductFolderName = "VoicePrompt";

    public AppPaths(string root)
    {
        Root = root;
        TokensFile = Path.Combine(root, "tokens.bin");
        HistoryFile = Path.Combine(root, "history.json");
        SettingsFile = Path.Combine(root, "settings.json");
        RecoveryDirectory = Path.Combine(root, "dictation-recovery");
        LogDirectory = Path.Combine(root, "logs");
        LogFile = Path.Combine(LogDirectory, "voiceprompt.log");
    }

    /// <summary>Root state directory (<c>%LOCALAPPDATA%\VoicePrompt</c>).</summary>
    public string Root { get; }

    /// <summary>DPAPI-encrypted OAuth token cache.</summary>
    public string TokensFile { get; }

    /// <summary>Bounded local transcript history (text only, never audio).</summary>
    public string HistoryFile { get; }

    /// <summary>User settings (backend URL, notification pause, auto-start intent).</summary>
    public string SettingsFile { get; }

    public string RecoveryDirectory { get; }

    public string LogDirectory { get; }

    public string LogFile { get; }

    /// <summary>The default per-user location under LocalAppData.</summary>
    public static AppPaths Default() => new(Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        ProductFolderName));

    /// <summary>Ensure the state directories exist.</summary>
    public void EnsureCreated(IFileSystem fs)
    {
        fs.CreateDirectory(Root);
        fs.CreateDirectory(LogDirectory);
    }
}
