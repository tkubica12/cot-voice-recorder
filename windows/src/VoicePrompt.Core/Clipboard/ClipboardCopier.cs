namespace VoicePrompt.Core.Clipboard;

/// <summary>
/// Writes text to the Windows clipboard. The real implementation must run on an STA thread;
/// <c>OpenClipboard</c> can transiently fail while another process owns the clipboard, so it
/// is allowed (and expected) to throw and be retried by <see cref="ClipboardCopier"/>.
/// </summary>
public interface IClipboardWriter
{
    void SetText(string text);
}

/// <summary>Outcome of a copy attempt.</summary>
public enum ClipboardCopyResult
{
    /// <summary>Text was placed on the clipboard.</summary>
    Copied,

    /// <summary>Every bounded attempt failed (clipboard locked by another process).</summary>
    Failed,

    /// <summary>Nothing to copy.</summary>
    Empty,
}

/// <summary>
/// Copies transcript text to the clipboard with bounded retries. Windows serialises clipboard
/// access, so a transient <c>CLIPBRD_E_CANT_OPEN</c> is common and must not lose the
/// transcript — history always keeps a copy the user can re-copy manually.
/// </summary>
public sealed class ClipboardCopier
{
    private readonly IClipboardWriter _writer;
    private readonly int _maxAttempts;
    private readonly Func<int, TimeSpan> _backoff;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;

    public ClipboardCopier(
        IClipboardWriter writer,
        int maxAttempts = 5,
        Func<int, TimeSpan>? backoff = null,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        if (maxAttempts < 1)
        {
            throw new ArgumentOutOfRangeException(nameof(maxAttempts));
        }

        _writer = writer;
        _maxAttempts = maxAttempts;
        _backoff = backoff ?? (attempt => TimeSpan.FromMilliseconds(Math.Min(500, 50 * attempt)));
        _delay = delay ?? Task.Delay;
    }

    public int MaxAttempts => _maxAttempts;

    /// <summary>Attempt the copy, retrying a bounded number of times on failure.</summary>
    public async Task<ClipboardCopyResult> CopyAsync(string? text, CancellationToken ct)
    {
        if (string.IsNullOrEmpty(text))
        {
            return ClipboardCopyResult.Empty;
        }

        for (var attempt = 1; attempt <= _maxAttempts; attempt++)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                _writer.SetText(text);
                return ClipboardCopyResult.Copied;
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch
            {
                if (attempt == _maxAttempts)
                {
                    return ClipboardCopyResult.Failed;
                }

                await _delay(_backoff(attempt), ct).ConfigureAwait(false);
            }
        }

        return ClipboardCopyResult.Failed;
    }
}
