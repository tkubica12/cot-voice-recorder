using System.Text.Json;
using VoicePrompt.Core.Api;

namespace VoicePrompt.Core.Dictation;

public sealed record DictationProgress(long CapturedBytes, long SavedBytes, long TranscribedBytes,
    int PendingChunks, string Transcript, string Preview, Exception? ServiceError, Exception? Failure);

/// <summary>One audio producer; durable disk backlog; at most two transcription requests in memory.</summary>
public sealed class RecoverableDictationSession : IAsyncDisposable
{
    private readonly object _gate = new();
    private readonly RecoveryJournal _journal;
    private readonly Func<byte[], CancellationToken, Task<string>> _transcribe;
    private readonly Func<Exception, bool> _retryable;
    private readonly TimeSpan _retryDelay;
    private readonly CancellationTokenSource _cancel = new();
    private readonly SemaphoreSlim _wake = new(0, 1);
    private readonly Dictionary<int, Task> _running = new();
    private readonly Dictionary<int, (Exception Error, DateTimeOffset RetryAt, int Attempts)> _errors = new();
    private readonly Task _pump;
    private readonly PcmChunker _chunker;
    private RecoveryState _state;
    private long _capturedBytes;
    private Exception? _failure;
    private DictationProgress _progress = new(0, 0, 0, 0, "", "", null, null);

    public RecoverableDictationSession(RecoveryJournal journal,
        Func<byte[], CancellationToken, Task<string>> transcribe,
        Func<Exception, bool>? retryable = null, TimeSpan? retryDelay = null)
    {
        _journal = journal;
        _transcribe = transcribe;
        _retryable = retryable ?? IsRetryable;
        _retryDelay = retryDelay ?? TimeSpan.FromSeconds(2);
        if (_retryDelay <= TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(retryDelay));
        _state = journal.Snapshot;
        _capturedBytes = _state.CapturedBytes;
        _chunker = _state.Tail.Length == 0 ? new PcmChunker() :
            new PcmChunker(JsonSerializer.Deserialize<PcmChunkerState>(_state.Tail)
                ?? throw new InvalidDataException("Missing audio checkpoint state."));
        Publish();
        _pump = Task.Run(PumpAsync);
    }

    public string Id => _state.Id;
    public DictationProgress Progress => Volatile.Read(ref _progress) with
    {
        CapturedBytes = Interlocked.Read(ref _capturedBytes),
    };

    public void Append(ReadOnlySpan<byte> pcm)
    {
        _cancel.Token.ThrowIfCancellationRequested();
        if (_state.CaptureComplete) throw new InvalidOperationException("Capture is already complete.");
        if (Progress.Failure is { } error) throw new IOException("Recovery storage failed.", error);
        _capturedBytes += pcm.Length;
        var chunks = _chunker.Append(pcm);
        if (chunks.Count > 0 || _capturedBytes - Progress.SavedBytes >= PcmChunker.BytesPerSecond * 2)
            Checkpoint(chunks, complete: false);
    }

    public void FinishCapture()
    {
        if (_state.CaptureComplete) return;
        if (Progress.Failure is { } error) throw new IOException("Recovery storage failed.", error);
        Checkpoint(_chunker.Flush(), complete: true);
    }

    private void Checkpoint(IReadOnlyList<AudioChunk> chunks, bool complete)
    {
        var tail = complete ? [] : JsonSerializer.SerializeToUtf8Bytes(_chunker.Snapshot());
        try
        {
            _journal.Checkpoint(tail, _capturedBytes, chunks, complete);
            lock (_gate)
            {
                _state = _journal.Snapshot;
                Publish();
                Wake();
            }
        }
        catch (Exception ex)
        {
            SetFailure(ex);
            throw;
        }
        finally
        {
            Array.Clear(tail);
            foreach (var chunk in chunks) Array.Clear(chunk.Wav);
        }
    }

    public async Task<string?> WaitForCompletionAsync(TimeSpan timeout, CancellationToken ct,
        bool resetTimeoutOnProgress = false)
    {
        var deadline = DateTimeOffset.UtcNow + timeout;
        var previousPending = Progress.PendingChunks;
        while (true)
        {
            ct.ThrowIfCancellationRequested();
            var progress = Progress;
            if (progress.Failure is { } failure) throw new IOException("Recovery storage failed.", failure);
            if (progress.PendingChunks == 0) return progress.Transcript;
            if (resetTimeoutOnProgress && progress.PendingChunks < previousPending)
                deadline = DateTimeOffset.UtcNow + timeout;
            previousPending = progress.PendingChunks;
            if (DateTimeOffset.UtcNow >= deadline) return null;
            await Task.Delay(50, ct).ConfigureAwait(false);
        }
    }

    private async Task PumpAsync()
    {
        try
        {
            while (!_cancel.IsCancellationRequested)
            {
                lock (_gate)
                {
                    if (_failure is null && _errors.Values.All(e => e.RetryAt <= DateTimeOffset.UtcNow))
                    {
                        foreach (var chunk in _state.Chunks.Where(c => c.Text is null))
                        {
                            if (_running.Count >= 2) break;
                            if (_running.ContainsKey(chunk.Index)
                                || (_errors.TryGetValue(chunk.Index, out var error) && error.RetryAt > DateTimeOffset.UtcNow))
                                continue;
                            var index = chunk.Index;
                            _running.Add(index, Task.Run(() => TranscribeAsync(index)));
                        }
                    }
                }
                await _wake.WaitAsync(TimeSpan.FromMilliseconds(250), _cancel.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) when (_cancel.IsCancellationRequested) { }
        catch (Exception ex) { SetFailure(ex); }
    }

    private async Task TranscribeAsync(int index)
    {
        byte[]? audio = null;
        try
        {
            try { audio = _journal.ReadAudio(index); }
            catch (Exception ex) { SetFailure(ex); return; }
            string text;
            try
            {
                text = await _transcribe(audio, _cancel.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (_cancel.IsCancellationRequested) { return; }
            catch (Exception ex)
            {
                lock (_gate)
                {
                    var attempts = _errors.TryGetValue(index, out var previous) ? previous.Attempts + 1 : 1;
                    var delay = TimeSpan.FromMilliseconds(Math.Min(30000, _retryDelay.TotalMilliseconds * Math.Min(attempts, 15)));
                    _errors[index] = (ex, _retryable(ex) ? DateTimeOffset.UtcNow + delay : DateTimeOffset.MaxValue, attempts);
                    Publish();
                }
                return;
            }
            try
            {
                _journal.SaveResult(index, text);
                lock (_gate)
                {
                    _state = _journal.Snapshot;
                    _errors.Remove(index);
                    Publish();
                }
            }
            catch (Exception ex) { SetFailure(ex); }
        }
        finally
        {
            if (audio is not null) Array.Clear(audio);
            lock (_gate)
            {
                _running.Remove(index);
                Wake();
            }
        }
    }

    private void Publish()
    {
        var prefix = _state.Chunks.TakeWhile(c => c.Text is not null).ToArray();
        var text = DictationSession.Stitch(prefix.Select(c => (c.Text!, c.OverlapsPrevious)));
        var pending = _state.Chunks.Count(c => c.Text is null);
        var through = _state.CaptureComplete && pending == 0 ? _state.CapturedBytes : prefix.LastOrDefault()?.EndByte ?? 0;
        Volatile.Write(ref _progress, new(_capturedBytes, _state.CapturedBytes, through, pending,
            text, PreviewTail(text), _errors.Values.FirstOrDefault().Error, _failure));
    }

    public static string PreviewTail(string text)
    {
        var words = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        var tail = string.Join(' ', words.TakeLast(30));
        var runes = tail.EnumerateRunes().ToArray();
        if (runes.Length > 220) tail = string.Concat(runes.TakeLast(220).Select(r => r.ToString()));
        return words.Length > 30 || runes.Length > 220 ? "... " + tail : tail;
    }

    private void SetFailure(Exception ex)
    {
        lock (_gate)
        {
            _failure ??= ex;
            Publish();
        }
    }

    private void Wake()
    {
        if (_wake.CurrentCount == 0) _wake.Release();
    }

    private static bool IsRetryable(Exception ex) => ex is HttpRequestException or TimeoutException or OperationCanceledException
        || ex is ApiException { Kind: ApiErrorKind.Retryable };

    public async ValueTask DisposeAsync()
    {
        _cancel.Cancel();
        await _pump.ConfigureAwait(false);
        Task[] running;
        lock (_gate) running = _running.Values.ToArray();
        await Task.WhenAll(running).ConfigureAwait(false);
        _chunker.Dispose();
        Array.Clear(_state.Tail);
        _journal.Dispose();
        _wake.Dispose();
        _cancel.Dispose();
    }
}
