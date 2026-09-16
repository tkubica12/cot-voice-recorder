using System.Text;

namespace VoicePrompt.Core.Dictation;

/// <summary>Single-producer session; uploads run concurrently, output always follows capture order.</summary>
public sealed class DictationSession : IAsyncDisposable
{
    public const int MaxPendingChunks = 8;
    private readonly Func<byte[], CancellationToken, Task<string>> _transcribe;
    private readonly CancellationTokenSource _cancel = new();
    private readonly SemaphoreSlim _slots = new(2);
    private readonly List<(bool Overlap, Task<string> Text)> _chunks = new();
    private bool _complete;

    public DictationSession(Func<byte[], CancellationToken, Task<string>> transcribe) =>
        _transcribe = transcribe;

    public Exception? Failure => _chunks.FirstOrDefault(c => c.Text.IsFaulted).Text?.Exception?.GetBaseException();
    public int PendingChunks => _chunks.Count(c => !c.Text.IsCompleted);

    public void Add(AudioChunk chunk)
    {
        if (_complete)
            throw new InvalidOperationException("Dictation is already stopped.");
        _cancel.Token.ThrowIfCancellationRequested();
        if (Failure is { } failure)
            throw new InvalidOperationException("A dictation chunk failed. Nothing was pasted.", failure);
        if (PendingChunks >= MaxPendingChunks)
            throw new InvalidOperationException("Transcription cannot keep up. Nothing was pasted; try a shorter dictation.");
        _chunks.Add((chunk.OverlapsPrevious, RunAsync(chunk.Wav)));
    }

    private async Task<string> RunAsync(byte[] wav)
    {
        try
        {
            await _slots.WaitAsync(_cancel.Token).ConfigureAwait(false);
            try
            {
                return await _transcribe(wav, _cancel.Token).ConfigureAwait(false);
            }
            finally
            {
                _slots.Release();
            }
        }
        finally
        {
            Array.Clear(wav);
        }
    }

    public async Task<string> CompleteAsync()
    {
        _complete = true;
        await Task.WhenAll(_chunks.Select(c => c.Text)).ConfigureAwait(false);
        return Stitch(_chunks.Select(c => (c.Text.Result, c.Overlap)));
    }

    public void Cancel() => _cancel.Cancel();

    public async ValueTask DisposeAsync()
    {
        _cancel.Cancel();
        try
        {
            await Task.WhenAll(_chunks.Select(c => c.Text)).ConfigureAwait(false);
        }
        catch (Exception) when (_chunks.Any(c => c.Text.IsFaulted || c.Text.IsCanceled))
        {
            // All tasks are observed here; the owner reports Failure/CompleteAsync errors.
        }
        _slots.Dispose();
        _cancel.Dispose();
    }

    public static string Stitch(IEnumerable<(string Text, bool Overlap)> chunks)
    {
        var tokens = new List<string>();
        var previousTokenCount = 0;
        foreach (var (text, overlap) in chunks)
        {
            var incoming = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
            var skip = 0;
            if (overlap)
            {
                for (var count = Math.Min(12, Math.Min(previousTokenCount, Math.Min(tokens.Count, incoming.Length))); count > 0; count--)
                {
                    var tail = tokens.TakeLast(count).Select(Normalize).ToArray();
                    var head = incoming.Take(count).Select(Normalize);
                    if (tail.SequenceEqual(head) && tail.Any(t => t.Length > 0))
                    {
                        skip = count;
                        break;
                    }
                }
            }
            tokens.AddRange(incoming.Skip(skip));
            previousTokenCount = incoming.Length;
        }
        return string.Join(' ', tokens);
    }

    private static string Normalize(string token) =>
        new(token.Normalize(NormalizationForm.FormKC).ToLowerInvariant()
            .Where(c => char.IsLetterOrDigit(c) || c == '_').ToArray());
}
