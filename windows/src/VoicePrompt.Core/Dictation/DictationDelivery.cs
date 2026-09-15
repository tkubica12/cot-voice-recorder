using VoicePrompt.Core.Clipboard;

namespace VoicePrompt.Core.Dictation;

public enum DictationDeliveryResult { PasteSent, CopiedOnly, ClipboardFailed, Empty }

public interface IDictationDesktop
{
    bool IsTargetUnchanged { get; }
    bool AreModifiersReleased { get; }
    bool TrySendPaste();
}

public static class DictationDelivery
{
    public static async Task<DictationDeliveryResult> DeliverAsync(
        string text, IDictationDesktop desktop, ClipboardCopier clipboard, CancellationToken ct,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        delay ??= Task.Delay;
        if (string.IsNullOrWhiteSpace(text))
            return DictationDeliveryResult.Empty;
        var copied = await clipboard.CopyAsync(text, ct);
        if (copied != ClipboardCopyResult.Copied)
            return DictationDeliveryResult.ClipboardFailed;
        for (var i = 0; i < 30; i++)
        {
            ct.ThrowIfCancellationRequested();
            if (!desktop.IsTargetUnchanged)
                return DictationDeliveryResult.CopiedOnly;
            if (desktop.AreModifiersReleased)
                return desktop.TrySendPaste()
                    ? DictationDeliveryResult.PasteSent : DictationDeliveryResult.CopiedOnly;
            await delay(TimeSpan.FromMilliseconds(25), ct);
        }
        return DictationDeliveryResult.CopiedOnly;
    }
}
