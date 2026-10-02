using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Dictation;

public sealed class DictationAuditOutbox(
    RecoveryStore recovery, HistoryStore history, IClock clock,
    Func<string> context, Func<bool> canUpload,
    Func<RecoveryState, CancellationToken, Task> upload,
    ILog log, Action? failed = null)
{
    private readonly SemaphoreSlim _gate = new(1, 1);

    public async Task DrainAsync(CancellationToken ct)
    {
        if (!canUpload() || !await _gate.WaitAsync(0, ct).ConfigureAwait(false)) return;
        try
        {
            foreach (var info in recovery.List().Where(i => i.Error is null && i.AuditPending))
            {
                ct.ThrowIfCancellationRequested();
                if (!canUpload()) return;
                using var journal = recovery.Open(info.Id);
                var state = journal.Snapshot;
                try
                {
                    if (state.Context != context() || state.AuditText is null || state.AuditCompletedAt is null)
                        continue;
                    if (clock.UtcNow - state.AuditCompletedAt >= HistoryStore.Retention)
                    {
                        journal.Delete();
                        log.Warn("dictation audit: expired before cloud acknowledgement; local history retention applies");
                        failed?.Invoke();
                        continue;
                    }
                    var id = Guid.ParseExact(state.Id, "N").ToString("D");
                    if (history.Get(id) is null)
                        history.Add(new HistoryEntry
                        {
                            TranscriptId = id, RecordingId = id, Body = state.AuditText,
                            RawBody = state.AuditPolished ? state.ConfirmedText : null,
                            Preview = RecoverableDictationSession.PreviewTail(state.AuditText),
                            CompletedAt = state.AuditCompletedAt.Value, CachedAt = clock.UtcNow,
                            CharacterCount = state.AuditText.Length,
                        });
                    await upload(state, ct).ConfigureAwait(false);
                    if (state.Context != context() || !canUpload()) continue;
                    journal.Delete();
                    log.Info("dictation audit: cloud acknowledged; encrypted outbox removed");
                }
                catch (OperationCanceledException) when (ct.IsCancellationRequested) { throw; }
                catch (Exception ex)
                {
                    log.Warn($"dictation audit: pending ({ex.GetType().Name}); checkpoint retained");
                    failed?.Invoke();
                }
                finally { Array.Clear(state.Tail); }
            }
        }
        finally { _gate.Release(); }
    }
}
