using VoicePrompt.Core.Api;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;
using VoicePrompt.Core.Realtime;

namespace VoicePrompt.Core;

/// <summary>What the coordinator did with one <c>transcript.completed</c> event.</summary>
public enum TranscriptHandlingResult
{
    /// <summary>Fetched, cached, copied to the clipboard and announced.</summary>
    CopiedAndNotified,

    /// <summary>Fetched and cached, but notifications are paused so nothing was copied or shown.</summary>
    CachedWhilePaused,

    /// <summary>The <c>event_id</c> was already handled.</summary>
    Duplicate,

    /// <summary>Fetched and cached, but the clipboard could not be written after retries.</summary>
    ClipboardFailed,

    /// <summary>The transcript could not be fetched (404 / auth / transient exhausted).</summary>
    FetchFailed,
}

/// <summary>
/// Turns a realtime <c>transcript.completed</c> event into the user-visible outcome:
/// dedupe → authenticated fetch → local cache → clipboard → tray notification.
///
/// When notifications are paused the transcript is still fetched and cached (so
/// "Copy latest" and history work), but nothing is copied automatically and no toast is
/// shown. Full transcript text never reaches the logs or the notification body.
/// </summary>
public sealed class TranscriptCoordinator
{
    private readonly ApiClient _api;
    private readonly HistoryStore _history;
    private readonly ClipboardCopier _clipboard;
    private readonly INotifier _notifier;
    private readonly EventDeduplicator _dedupe;
    private readonly IClock _clock;
    private readonly ILog _log;
    private readonly Func<bool> _notificationsPaused;

    public TranscriptCoordinator(
        ApiClient api,
        HistoryStore history,
        ClipboardCopier clipboard,
        INotifier notifier,
        IClock clock,
        Func<bool> notificationsPaused,
        EventDeduplicator? dedupe = null,
        ILog? log = null)
    {
        _api = api;
        _history = history;
        _clipboard = clipboard;
        _notifier = notifier;
        _clock = clock;
        _notificationsPaused = notificationsPaused;
        _dedupe = dedupe ?? new EventDeduplicator();
        _log = log ?? NullLog.Instance;
    }

    /// <summary>Raised after an event has been handled, so the UI can refresh.</summary>
    public event Action<TranscriptHandlingResult>? Handled;

    public EventDeduplicator Deduplicator => _dedupe;

    public async Task<TranscriptHandlingResult> HandleAsync(TranscriptCompletedEvent evt, CancellationToken ct)
    {
        if (!_dedupe.TryMarkSeen(evt.EventId))
        {
            _log.Debug("event: duplicate transcript.completed ignored");
            return Finish(TranscriptHandlingResult.Duplicate);
        }

        Transcript transcript;
        try
        {
            transcript = await _api.GetTranscriptAsync(evt.TranscriptId, ct).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch (ApiException ex)
        {
            _log.Warn($"event: transcript fetch failed ({ex.Kind}/{ex.StatusCode})");
            _notifier.Notify("VoicePrompt", "Could not fetch the transcript.", NotificationKind.Warning);
            return Finish(TranscriptHandlingResult.FetchFailed);
        }

        _history.Add(new HistoryEntry
        {
            TranscriptId = transcript.TranscriptId,
            RecordingId = transcript.RecordingId,
            Preview = transcript.Preview,
            Body = transcript.Body,
            CompletedAt = transcript.CompletedAt == default ? _clock.UtcNow : transcript.CompletedAt,
            CachedAt = _clock.UtcNow,
            CharacterCount = transcript.CharacterCount,
        });

        if (_notificationsPaused())
        {
            _log.Info("event: transcript cached (notifications paused; no auto-copy)");
            return Finish(TranscriptHandlingResult.CachedWhilePaused);
        }

        var copy = await _clipboard.CopyAsync(transcript.Body, ct).ConfigureAwait(false);
        if (copy != ClipboardCopyResult.Copied)
        {
            _log.Warn("event: clipboard write failed after retries; transcript kept in history");
            _notifier.Notify(
                "VoicePrompt",
                "Transcript ready, but the clipboard was busy. Use \"Copy latest\".",
                NotificationKind.Warning);
            return Finish(TranscriptHandlingResult.ClipboardFailed);
        }

        _log.Info($"event: transcript copied ({transcript.CharacterCount} chars)");
        _notifier.Notify("Transcript copied", Summarize(transcript.Preview));
        return Finish(TranscriptHandlingResult.CopiedAndNotified);
    }

    /// <summary>Copy the newest cached transcript on demand (works while paused).</summary>
    public async Task<ClipboardCopyResult> CopyLatestAsync(CancellationToken ct)
    {
        var latest = _history.Latest();
        if (latest is null)
        {
            return ClipboardCopyResult.Empty;
        }

        return await _clipboard.CopyAsync(latest.Body, ct).ConfigureAwait(false);
    }

    /// <summary>Copy a specific cached transcript on demand (works while paused).</summary>
    public Task<ClipboardCopyResult> CopyAsync(string transcriptId, CancellationToken ct)
    {
        var entry = _history.Get(transcriptId);
        return entry is null
            ? Task.FromResult(ClipboardCopyResult.Empty)
            : _clipboard.CopyAsync(entry.Body, ct);
    }

    public Task<ClipboardCopyResult> CopyOriginalAsync(string transcriptId, CancellationToken ct)
    {
        var original = _history.Get(transcriptId)?.RawBody;
        return original is null
            ? Task.FromResult(ClipboardCopyResult.Empty)
            : _clipboard.CopyAsync(original, ct);
    }

    private TranscriptHandlingResult Finish(TranscriptHandlingResult result)
    {
        Handled?.Invoke(result);
        return result;
    }

    /// <summary>Trim the short preview for a balloon tip; never includes the full body.</summary>
    internal static string Summarize(string? preview)
    {
        var text = (preview ?? string.Empty).Trim();
        if (text.Length == 0)
        {
            return "Ready on the clipboard.";
        }

        return text.Length <= 120 ? text : text[..119] + "…";
    }
}
