using Microsoft.Win32;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.App.Services;

/// <summary>
/// Per-user auto-start via <c>HKCU\Software\Microsoft\Windows\CurrentVersion\Run</c>. This is
/// the correct mechanism for a non-Store, per-user install: no elevation, no scheduled task,
/// and it is removed with the user profile.
/// </summary>
public sealed class RegistryAutoStartManager : IAutoStartManager
{
    private const string RunKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
    public const string ValueName = "VoicePrompt";

    private readonly string _executablePath;
    private readonly ILog _log;

    public RegistryAutoStartManager(string executablePath, ILog? log = null)
    {
        _executablePath = executablePath;
        _log = log ?? NullLog.Instance;
    }

    public bool IsEnabled
    {
        get
        {
            try
            {
                using var key = Registry.CurrentUser.OpenSubKey(RunKeyPath, writable: false);
                return key?.GetValue(ValueName) is string value && value.Length > 0;
            }
            catch (Exception ex)
            {
                _log.Warn($"autostart: read failed ({ex.GetType().Name})");
                return false;
            }
        }
    }

    public bool Set(bool enabled)
    {
        try
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKeyPath, writable: true);
            if (key is null)
            {
                return false;
            }

            if (enabled)
            {
                key.SetValue(ValueName, $"\"{_executablePath}\"", RegistryValueKind.String);
            }
            else
            {
                key.DeleteValue(ValueName, throwOnMissingValue: false);
            }

            _log.Info($"autostart: {(enabled ? "enabled" : "disabled")}");
            return true;
        }
        catch (Exception ex)
        {
            _log.Warn($"autostart: write failed ({ex.GetType().Name})");
            return false;
        }
    }
}
