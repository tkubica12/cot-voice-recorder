namespace VoicePrompt.Core.Infrastructure;

/// <summary>
/// Per-user auto-start at sign-in. The production implementation writes the
/// <c>HKCU\Software\Microsoft\Windows\CurrentVersion\Run</c> value, which needs no elevation
/// and matches the per-user (non-Store) install model.
/// </summary>
public interface IAutoStartManager
{
    bool IsEnabled { get; }

    /// <summary>Enable or disable auto-start. Returns <c>true</c> when the change was applied.</summary>
    bool Set(bool enabled);
}

/// <summary>Auto-start manager that does nothing (tests / unsupported hosts).</summary>
public sealed class NullAutoStartManager : IAutoStartManager
{
    private bool _enabled;

    public bool IsEnabled => _enabled;

    public bool Set(bool enabled)
    {
        _enabled = enabled;
        return true;
    }
}
