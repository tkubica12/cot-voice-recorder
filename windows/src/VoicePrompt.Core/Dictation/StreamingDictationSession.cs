using System.Buffers.Binary;
using System.Diagnostics;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Dictation;

/// <summary>Live PCM is independent of durable capture; only completed commits release saved audio.</summary>
public sealed class StreamingDictationSession : IRecoverableDictationSession
{
    private const int CheckpointBytes = PcmChunker.BytesPerSecond * 2;
    private const int PacketBytes = PcmChunker.BytesPerSecond / 50;
    private readonly object _gate = new();
    private readonly RecoveryJournal _journal;
    private readonly IDictationStreamFactory? _factory;
    private readonly Func<byte[], CancellationToken, Task<string>> _fallback;
    private readonly ILog _log;
    private readonly CancellationTokenSource _cancel = new();
    private readonly SemaphoreSlim _wake = new(0, 1);
    private readonly byte[] _buffer = new byte[CheckpointBytes];
    private readonly Dictionary<string, Hypothesis> _hypotheses = new();
    private readonly Task _pump;
    private readonly bool _recovery;
    private RecoveryState _state;
    private long _captured;
    private int _length;
    private byte[]? _readCache;
    private long _cacheStart;
    private Exception? _failure;
    private Exception? _serviceError;
    private DictationProgress _progress = new(0, 0, 0, 0, "", "", null, null);

    private sealed class Hypothesis
    {
        internal string Delta = "";
        internal string Partial = "";
    }

    public StreamingDictationSession(RecoveryJournal journal, IDictationStreamFactory? factory,
        Func<byte[], CancellationToken, Task<string>> fallback, bool recovery = false, ILog? log = null)
    {
        _journal = journal;
        _factory = factory;
        _fallback = fallback;
        _log = log ?? NullLog.Instance;
        _recovery = recovery;
        _state = journal.Snapshot;
        if (!_state.Streaming) throw new InvalidDataException("Not a streaming recovery journal.");
        _captured = _state.CapturedBytes;
        Publish();
        _pump = Task.Run(PumpAsync);
    }

    public string Id => _state.Id;
    public DictationProgress Progress => Volatile.Read(ref _progress) with { CapturedBytes = Interlocked.Read(ref _captured) };

    public void Append(ReadOnlySpan<byte> pcm)
    {
        if (pcm.Length % 2 != 0) throw new InvalidDataException("Incomplete PCM sample.");
        lock (_gate)
        {
            _cancel.Token.ThrowIfCancellationRequested();
            if (_state.CaptureComplete) throw new InvalidOperationException("Capture is complete.");
            if (_failure is not null) throw new IOException("Recovery storage failed.", _failure);
            while (!pcm.IsEmpty)
            {
                var count = Math.Min(_buffer.Length - _length, pcm.Length);
                pcm[..count].CopyTo(_buffer.AsSpan(_length));
                _length += count;
                _captured += count;
                pcm = pcm[count..];
                if (_length == _buffer.Length) Checkpoint(complete: false);
            }
            Wake();
        }
    }

    public void FinishCapture()
    {
        lock (_gate)
        {
            if (!_state.CaptureComplete) Checkpoint(complete: true);
            Wake();
        }
    }

    private void Checkpoint(bool complete)
    {
        if (_failure is not null) throw new IOException("Recovery storage failed.", _failure);
        var audio = _length == 0 ? null : PcmChunker.ToWav(_buffer.AsSpan(0, _length));
        try
        {
            _journal.Checkpoint(_state.Tail, _captured,
                audio is null ? [] : [new AudioChunk(audio, false, _captured)], complete);
            ReplaceState();
            Array.Clear(_buffer);
            _length = 0;
            Publish();
        }
        catch (Exception ex)
        {
            _failure = ex;
            Publish();
            throw;
        }
        finally { if (audio is not null) Array.Clear(audio); }
    }

    private void ReplaceState()
    {
        var next = _journal.Snapshot;
        Array.Clear(_state.Tail);
        _state = next;
    }

    private byte[] ReadRange(long position, int maximum)
    {
        lock (_gate)
        {
            var sealedEnd = _state.Chunks.LastOrDefault()?.EndByte ?? 0;
            if (position >= sealedEnd)
            {
                var offset = checked((int)(position - sealedEnd));
                return offset < _length ? _buffer.AsSpan(offset, Math.Min(maximum, _length - offset)).ToArray() : [];
            }
            if (_readCache is null || position < _cacheStart || position >= _cacheStart + _readCache.Length - 44)
            {
                if (_readCache is not null) Array.Clear(_readCache);
                var chunk = _state.Chunks.FirstOrDefault(c => c.EndByte > position)
                    ?? throw new IOException("Missing recovery audio position.");
                try { _readCache = _journal.ReadAudio(chunk.Index); }
                catch (Exception ex)
                {
                    _failure = ex;
                    Publish();
                    throw;
                }
                _cacheStart = chunk.EndByte - (_readCache.Length - 44);
            }
            var start = checked((int)(position - _cacheStart));
            return _readCache.AsSpan(44 + start, Math.Min(maximum, _readCache.Length - 44 - start)).ToArray();
        }
    }

    private byte[] ContextAt(long through)
    {
        var start = Math.Max(0, through - PcmChunker.BytesPerSecond * PcmChunker.OverlapMilliseconds / 1000);
        using var bytes = new MemoryStream();
        while (start < through)
        {
            byte[] part;
            if (start < _state.ConfirmedBytes)
            {
                var priorStart = _state.ConfirmedBytes - _state.Tail.Length;
                if (start < priorStart) throw new IOException("Missing recovery context audio.");
                var offset = checked((int)(start - priorStart));
                part = _state.Tail.AsSpan(offset, checked((int)(Math.Min(through, _state.ConfirmedBytes) - start))).ToArray();
            }
            else part = ReadRange(start, checked((int)(through - start)));
            if (part.Length == 0) throw new IOException("Missing confirmed audio.");
            bytes.Write(part);
            start += part.Length;
            Array.Clear(part);
        }
        return bytes.ToArray();
    }

    private void Confirm(long through, string text, bool fallback = false, int segmentWords = -1)
    {
        lock (_gate)
        {
            if (through <= _state.ConfirmedBytes || through > _state.CapturedBytes)
                throw new InvalidDataException("Unexpected streaming confirmation cursor.");
            byte[]? context = null;
            try
            {
                context = ContextAt(through);
                _journal.SaveStreamingResult(through, text, context, fallback, segmentWords);
                ReplaceState();
                Publish();
            }
            catch (Exception ex)
            {
                _failure = ex;
                Publish();
                throw;
            }
            finally { if (context is not null) Array.Clear(context); }
        }
    }

    private async Task PumpAsync()
    {
        while (!_cancel.IsCancellationRequested)
        {
            try
            {
                if (_recovery)
                {
                    if (!_state.CaptureComplete)
                    {
                        await _wake.WaitAsync(TimeSpan.FromMilliseconds(100), _cancel.Token).ConfigureAwait(false);
                        continue;
                    }
                    await FallbackAsync(_cancel.Token).ConfigureAwait(false);
                    return;
                }
                if (_state.CaptureComplete && _state.ConfirmedBytes == _captured) return;
                if (_factory is null) throw new InvalidOperationException("Streaming is not configured.");
                await StreamAsync(_cancel.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (_cancel.IsCancellationRequested) { return; }
            catch (Exception ex)
            {
                lock (_gate)
                {
                    _serviceError = ex;
                    _hypotheses.Clear();
                    Publish();
                }
                _log.Warn($"dictation stream: interrupted ({ex.GetType().Name}); audio retained");
                if (_failure is not null) return;
                if (ex is ApiException { Kind: ApiErrorKind.Forbidden or ApiErrorKind.Unauthorized })
                {
                    try { await Task.Delay(Timeout.InfiniteTimeSpan, _cancel.Token).ConfigureAwait(false); }
                    catch (OperationCanceledException) when (_cancel.IsCancellationRequested) { }
                    return;
                }
                try { await Task.Delay(TimeSpan.FromSeconds(2), _cancel.Token).ConfigureAwait(false); }
                catch (OperationCanceledException) when (_cancel.IsCancellationRequested) { return; }
            }
        }
    }

    private async Task StreamAsync(CancellationToken ct)
    {
        long position;
        lock (_gate)
        {
            position = _state.ConfirmedBytes;
            _hypotheses.Clear();
            if (_readCache is not null) Array.Clear(_readCache);
            _readCache = null;
        }
        await using var stream = await _factory!.ConnectAsync(_state.Language, position, ct).ConfigureAwait(false);
        using var connection = CancellationTokenSource.CreateLinkedTokenSource(ct);
        var token = connection.Token;
        var age = Stopwatch.StartNew();

        async Task SendAsync()
        {
            var committed = position;
            var quietBytes = 0;
            while (true)
            {
                var audio = ReadRange(position, PacketBytes);
                if (audio.Length > 0)
                {
                    try
                    {
                        double energy = 0;
                        for (var offset = 0; offset < audio.Length; offset += 2)
                        {
                            var sample = BinaryPrimitives.ReadInt16LittleEndian(audio.AsSpan(offset));
                            energy += (double)sample * sample;
                        }
                        quietBytes = energy / (audio.Length / 2) < 120 * 120 ? quietBytes + audio.Length : 0;
                        await stream.SendAudioAsync(audio, token).ConfigureAwait(false);
                        position += audio.Length;
                    }
                    finally { Array.Clear(audio); }
                    if (quietBytes >= PcmChunker.BytesPerSecond * 0.4 && position - committed >= CheckpointBytes)
                    {
                        lock (_gate) { if (!_state.CaptureComplete) Checkpoint(complete: false); }
                        await stream.CommitAsync(false, token).ConfigureAwait(false);
                        committed = position;
                        quietBytes = 0;
                    }
                }
                else
                {
                    if (_state.CaptureComplete && position == _captured) break;
                    await _wake.WaitAsync(TimeSpan.FromMilliseconds(100), token).ConfigureAwait(false);
                }
                if (age.Elapsed >= TimeSpan.FromMinutes(50)) break;
            }
            lock (_gate) { if (!_state.CaptureComplete) Checkpoint(complete: false); }
            await stream.CommitAsync(true, token).ConfigureAwait(false);
        }

        async Task ReceiveAsync()
        {
            while (true)
            {
                var message = await stream.ReceiveAsync(token).ConfigureAwait(false);
                lock (_gate)
                {
                    _serviceError = null;
                    if (message.Type == "confirmed")
                    {
                        Confirm(message.ByteEnd, DictationSession.AppendPreservingPrefix(_state.ConfirmedText, message.Text, false),
                            segmentWords: message.Text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length);
                        _hypotheses.Remove(message.ItemId);
                    }
                    else if (message.Type == "done")
                    {
                        if (message.ByteEnd != _state.ConfirmedBytes)
                            throw new InvalidDataException("Stream completed with unconfirmed audio.");
                        Publish();
                        return;
                    }
                    else
                    {
                        if (!_hypotheses.TryGetValue(message.ItemId, out var hypothesis))
                        {
                            if (_hypotheses.Count >= 32) throw new InvalidDataException("Too many pending streaming items.");
                            _hypotheses.Add(message.ItemId, hypothesis = new());
                        }
                        if (message.Type == "delta")
                        {
                            hypothesis.Delta += message.Text;
                            hypothesis.Partial = "";
                        }
                        else hypothesis.Partial = message.Text;
                        if (hypothesis.Delta.Length + hypothesis.Partial.Length > 1_048_576)
                            throw new InvalidDataException("Streaming hypothesis exceeds supported bounds.");
                    }
                    Publish();
                }
            }
        }

        var sender = SendAsync();
        var receiver = ReceiveAsync();
        try
        {
            var finished = await Task.WhenAny(sender, receiver).ConfigureAwait(false);
            await finished.ConfigureAwait(false);
            await Task.WhenAll(sender, receiver).WaitAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
        }
        finally
        {
            connection.Cancel();
            try { await Task.WhenAll(sender, receiver).ConfigureAwait(false); }
            catch (OperationCanceledException) when (connection.IsCancellationRequested) { }
            catch (Exception ex) { _log.Warn($"dictation stream: closing interrupted transport ({ex.GetType().Name})"); }
        }
    }

    private async Task FallbackAsync(CancellationToken ct)
    {
        var originalCursor = _state.ConfirmedBytes;
        if (originalCursor == _captured) return;
        var context = _state.Tail.ToArray();
        var start = originalCursor - context.Length;
        using var chunker = new PcmChunker(silenceRms: 0);
        var first = true;
        async Task TranscribeAsync(IReadOnlyList<AudioChunk> chunks)
        {
            foreach (var chunk in chunks)
            {
                try
                {
                    var text = await _fallback(chunk.Wav, ct).ConfigureAwait(false);
                    lock (_gate)
                    {
                        Confirm(start + chunk.EndByte,
                            DictationSession.AppendPreservingPrefix(_state.ConfirmedText, text,
                                chunk.OverlapsPrevious || (first && context.Length > 0), _state.LastSegmentWords),
                            fallback: true, segmentWords: text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length);
                    }
                    first = false;
                }
                finally { Array.Clear(chunk.Wav); }
            }
        }
        try
        {
            await TranscribeAsync(chunker.Append(context)).ConfigureAwait(false);
            var position = originalCursor;
            while (position < _captured)
            {
                ct.ThrowIfCancellationRequested();
                var audio = ReadRange(position, CheckpointBytes);
                if (audio.Length == 0) throw new IOException("Missing pending recovery audio.");
                try
                {
                    position += audio.Length;
                    await TranscribeAsync(chunker.Append(audio)).ConfigureAwait(false);
                }
                finally { Array.Clear(audio); }
            }
            await TranscribeAsync(chunker.Flush()).ConfigureAwait(false);
        }
        finally { Array.Clear(context); }
    }

    public void PrepareAudit(string text, DateTimeOffset completedAt, bool polished)
    {
        lock (_gate)
        {
            _journal.PrepareAudit(text, completedAt, polished);
            ReplaceState();
        }
    }

    public async Task<string?> WaitForCompletionAsync(TimeSpan timeout, CancellationToken ct,
        bool resetTimeoutOnProgress = false)
    {
        var deadline = DateTimeOffset.UtcNow + timeout;
        var previous = Progress.TranscribedBytes;
        while (true)
        {
            ct.ThrowIfCancellationRequested();
            var progress = Progress;
            if (progress.Failure is { } failure) throw new IOException("Recovery storage failed.", failure);
            if (_state.CaptureComplete && progress.TranscribedBytes == progress.CapturedBytes)
                return progress.Transcript;
            if (resetTimeoutOnProgress && progress.TranscribedBytes > previous)
                deadline = DateTimeOffset.UtcNow + timeout;
            previous = progress.TranscribedBytes;
            if (DateTimeOffset.UtcNow >= deadline) return null;
            await Task.Delay(25, ct).ConfigureAwait(false);
        }
    }

    private void Publish()
    {
        var provisional = string.Join(" ", _hypotheses.Values.Select(h => h.Delta + h.Partial));
        var preview = DictationSession.AppendPreservingPrefix(_state.ConfirmedText, provisional, false);
        Volatile.Write(ref _progress, new(_captured, _state.CapturedBytes, _state.ConfirmedBytes,
            _state.Chunks.Count(c => c.Text is null), _state.ConfirmedText,
            RecoverableDictationSession.PreviewTail(preview), _serviceError, _failure));
    }

    private void Wake() { if (_wake.CurrentCount == 0) _wake.Release(); }

    public async ValueTask DisposeAsync()
    {
        _cancel.Cancel();
        await _pump.ConfigureAwait(false);
        lock (_gate)
        {
            Array.Clear(_buffer);
            Array.Clear(_state.Tail);
            if (_readCache is not null) Array.Clear(_readCache);
            _journal.Dispose();
        }
        _cancel.Dispose();
        _wake.Dispose();
    }
}
