namespace VoicePrompt.Core.Notifications;

/// <summary>Severity for a tray notification.</summary>
public enum NotificationKind
{
    Info,
    Warning,
    Error,
}

/// <summary>
/// Shows a tray balloon/toast. Implementations must show <b>metadata only</b> — the short
/// preview from the event, never the full transcript body.
/// </summary>
public interface INotifier
{
    void Notify(string title, string message, NotificationKind kind = NotificationKind.Info);
}

/// <summary>No-op notifier for tests and headless runs.</summary>
public sealed class NullNotifier : INotifier
{
    public static readonly NullNotifier Instance = new();
    public void Notify(string title, string message, NotificationKind kind = NotificationKind.Info) { }
}
