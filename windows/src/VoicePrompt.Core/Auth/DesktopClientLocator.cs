using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// Finds the Google <b>Desktop</b> OAuth client JSON at runtime. Search order:
/// <list type="number">
///   <item>the <c>VOICEPROMPT_GOOGLE_DESKTOP_CLIENT</c> environment variable,</item>
///   <item><c>%LOCALAPPDATA%\VoicePrompt\google-desktop-client.json</c> (user override),</item>
///   <item><c>google-desktop-client.json</c> next to the executable (installed by the setup),</item>
///   <item><c>windows/installer/staging/google-desktop-client.json</c> when running from the repo.</item>
/// </list>
/// When no file is found the app runs in a safe <see cref="AuthState.NotConfigured"/> state:
/// sign-in is disabled, no placeholder credentials are minted, and CI can build and test
/// without any secret material.
/// </summary>
public static class DesktopClientLocator
{
    public const string FileName = "google-desktop-client.json";
    public const string EnvironmentVariable = "VOICEPROMPT_GOOGLE_DESKTOP_CLIENT";

    /// <summary>Candidate paths in priority order.</summary>
    public static IEnumerable<string> CandidatePaths(string baseDirectory, string stateDirectory)
    {
        var fromEnv = Environment.GetEnvironmentVariable(EnvironmentVariable);
        if (!string.IsNullOrWhiteSpace(fromEnv))
        {
            yield return fromEnv;
        }

        yield return Path.Combine(stateDirectory, FileName);
        yield return Path.Combine(baseDirectory, FileName);
        yield return Path.Combine(baseDirectory, "Assets", FileName);

        // Running from a dev build: walk up to the repo's git-ignored staging folder.
        var dir = new DirectoryInfo(baseDirectory);
        for (var i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            yield return Path.Combine(dir.FullName, "installer", "staging", FileName);
            yield return Path.Combine(dir.FullName, "windows", "installer", "staging", FileName);
        }
    }

    /// <summary>
    /// Load the Desktop client configuration, or <c>null</c> if none is present/valid.
    /// Never logs or returns the client secret to callers other than the token client.
    /// </summary>
    public static DesktopClientConfig? TryLoad(
        string baseDirectory,
        string stateDirectory,
        IFileSystem fs,
        ILog? log = null)
    {
        var logger = log ?? NullLog.Instance;
        foreach (var path in CandidatePaths(baseDirectory, stateDirectory))
        {
            try
            {
                if (!fs.FileExists(path))
                {
                    continue;
                }

                var json = fs.ReadAllText(path);
                if (!DesktopClientConfig.LooksLikeDesktopClient(json))
                {
                    logger.Warn("oauth: found a client JSON that is not a Desktop client; ignoring");
                    continue;
                }

                logger.Info("oauth: Desktop client configuration loaded");
                return DesktopClientConfig.Parse(json);
            }
            catch (Exception ex)
            {
                logger.Warn($"oauth: candidate client JSON unusable ({ex.GetType().Name})");
            }
        }

        logger.Info("oauth: no Desktop client configuration found; sign-in disabled");
        return null;
    }
}
