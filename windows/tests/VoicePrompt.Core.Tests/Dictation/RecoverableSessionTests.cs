using System.Buffers.Binary;
using System.Collections.Concurrent;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public sealed class RecoverableSessionTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "VoicePrompt-stream-tests-" + Guid.NewGuid().ToString("N"));
    private readonly RecoveryStore _store;

    public RecoverableSessionTests() => _store = new RecoveryStore(_root, new FakeSecretProtector());

    [Theory]
    [InlineData(37)]
    [InlineData(32001)]
    [InlineData(191997)]
    [InlineData(224000)]
    [InlineData(300007)]
    public void Snapshot_preserves_frames_silence_overlap_and_exact_audio(int boundary)
    {
        var pcm = ChunkerTests.Pcm(1000, 0).Concat(ChunkerTests.Pcm(14000)).Concat(ChunkerTests.Pcm(500, 0)).ToArray();
        var uninterrupted = new PcmChunker();
        var expected = uninterrupted.Append(pcm).Concat(uninterrupted.Flush()).ToArray();
        var beforeCrash = new PcmChunker();
        var first = beforeCrash.Append(pcm.AsSpan(0, boundary));
        var restored = new PcmChunker(beforeCrash.Snapshot());
        var actual = first.Concat(restored.Append(pcm.AsSpan(boundary))).Concat(restored.Flush()).ToArray();
        Assert.Equal(expected.Length, actual.Length);
        for (var i = 0; i < expected.Length; i++)
        {
            Assert.Equal(expected[i].Wav, actual[i].Wav);
            Assert.Equal(expected[i].OverlapsPrevious, actual[i].OverlapsPrevious);
            Assert.Equal(expected[i].EndByte, actual[i].EndByte);
        }
    }

    [Fact]
    public void Invalid_checkpoint_is_rejected()
    {
        var state = new PcmChunker().Snapshot();
        Assert.Throws<InvalidDataException>(() => new PcmChunker(state with { Audio = new byte[192000] }));
        Assert.Throws<InvalidDataException>(() => new PcmChunker(state with { Frame = new byte[640] }));
        Assert.Throws<InvalidDataException>(() => new PcmChunker(state with { NewBytes = 1 }));
        Assert.Throws<InvalidDataException>(() => new PcmChunker(state with { Version = 2 }));
    }

    [Fact]
    public async Task Two_requests_out_of_order_preview_only_exposes_contiguous_results()
    {
        var journal = _store.Create("auto", "account/backend");
        var chunks = Enumerable.Range(0, 3).Select(i =>
            new AudioChunk(PcmChunker.ToWav(ChunkerTests.Pcm(100, (short)(1200 + i))), false, (i + 1) * 3200)).ToArray();
        journal.Checkpoint([], 9600, chunks, true);
        var signals = Enumerable.Range(0, 3).Select(_ => new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously)).ToArray();
        var called = new ConcurrentDictionary<int, bool>();
        await using var session = new RecoverableDictationSession(journal, (wav, ct) =>
        {
            var index = BinaryPrimitives.ReadInt16LittleEndian(wav.AsSpan(44)) - 1200;
            called[index] = true;
            return signals[index].Task.WaitAsync(ct);
        });
        await UntilAsync(() => called.Count == 2);
        Assert.False(called.ContainsKey(2));
        signals[1].SetResult("second");
        await UntilAsync(() => called.ContainsKey(2));
        Assert.Equal("", session.Progress.Preview);
        Assert.Equal(0, session.Progress.TranscribedBytes);
        signals[2].SetResult("third");
        signals[0].SetResult("first");
        Assert.Equal("first second third", await session.WaitForCompletionAsync(TimeSpan.FromSeconds(5), CancellationToken.None));
        Assert.Equal(9600, session.Progress.TranscribedBytes);
        Assert.All(_store.Open(session.Id).Snapshot.Chunks, c => Assert.NotNull(c.Text));
    }

    [Fact]
    public async Task Short_overlap_matches_apply_to_preview_final_text_and_reopened_results()
    {
        var journal = _store.Create("auto");
        var id = journal.Snapshot.Id;
        journal.Checkpoint([], 44800,
            [new(PcmChunker.ToWav(ChunkerTests.Pcm(1000, 1200)), false, 32000),
             new(PcmChunker.ToWav(ChunkerTests.Pcm(1000, 1201)), true, 44800)], true);
        await using (var session = new RecoverableDictationSession(journal, (wav, _) =>
            Task.FromResult(BinaryPrimitives.ReadInt16LittleEndian(wav.AsSpan(44)) == 1200
                ? "send it to" : "to the editor")))
        {
            Assert.Equal("send it to the editor",
                await session.WaitForCompletionAsync(TimeSpan.FromSeconds(5), CancellationToken.None));
            Assert.Equal("send it to the editor", session.Progress.Preview);
            Assert.Equal("to the editor", journal.Snapshot.Chunks[1].Text);
        }
        await using var reopened = new RecoverableDictationSession(_store.Open(id), (_, _) =>
            throw new InvalidOperationException("Completed chunks must not be retranscribed."));
        Assert.Equal("send it to the editor",
            await reopened.WaitForCompletionAsync(TimeSpan.FromSeconds(1), CancellationToken.None));
    }

    [Fact]
    public async Task Twenty_minutes_offline_is_durable_bounded_and_recovers_in_order()
    {
        var journal = _store.Create("cs", "account/backend");
        var id = journal.Snapshot.Id;
        var calls = 0;
        var maxActive = 0;
        var active = 0;
        await using (var session = new RecoverableDictationSession(journal, async (_, ct) =>
        {
            Interlocked.Increment(ref calls);
            var concurrent = Interlocked.Increment(ref active);
            Interlocked.Exchange(ref maxActive, Math.Max(maxActive, concurrent));
            try { await Task.Delay(Timeout.Infinite, ct); return ""; }
            finally { Interlocked.Decrement(ref active); }
        }))
        {
            var frame = ChunkerTests.Pcm(1000);
            for (var second = 0; second < 20 * 60; second++)
                session.Append(frame);
            session.FinishCapture();
            await UntilAsync(() => Volatile.Read(ref calls) == 2);
            Assert.Equal(20L * 60 * PcmChunker.BytesPerSecond, session.Progress.SavedBytes);
            Assert.Equal(session.Progress.CapturedBytes, session.Progress.SavedBytes);
            Assert.True(session.Progress.PendingChunks > 200);
            Assert.Equal(2, maxActive);
            Assert.Null(await session.WaitForCompletionAsync(TimeSpan.FromMilliseconds(50), CancellationToken.None));
        }
        Assert.Equal(0, active);
        var reopened = _store.Open(id);
        var count = reopened.Snapshot.Chunks.Count;
        var recoveredCalls = 0;
        await using var recovered = new RecoverableDictationSession(reopened, (_, _) =>
        {
            return Task.FromResult("word" + Interlocked.Increment(ref recoveredCalls));
        });
        recovered.FinishCapture();
        var text = await recovered.WaitForCompletionAsync(TimeSpan.FromSeconds(30), CancellationToken.None,
            resetTimeoutOnProgress: true);
        Assert.NotNull(text);
        Assert.Equal(count, text.Split(' ').Length);
        Assert.Equal(count, recoveredCalls);
        Assert.Equal(0, recovered.Progress.PendingChunks);
    }

    [Fact]
    public async Task Crash_restores_only_durable_cursor_and_does_not_restart_chunk_boundaries()
    {
        var journal = _store.Create("auto");
        var id = journal.Snapshot.Id;
        await using (var session = new RecoverableDictationSession(journal, async (_, ct) =>
        {
            await Task.Delay(Timeout.Infinite, ct);
            return "";
        }))
        {
            for (var second = 0; second < 9; second++) session.Append(ChunkerTests.Pcm(1000));
            Assert.Equal(8L * PcmChunker.BytesPerSecond, session.Progress.SavedBytes);
            Assert.Equal(9L * PcmChunker.BytesPerSecond, session.Progress.CapturedBytes);
            // Disposal models a crash: no final flush/checkpoint.
        }
        var expectedChunker = new PcmChunker();
        var expected = expectedChunker.Append(ChunkerTests.Pcm(8000)).Concat(expectedChunker.Flush()).ToArray();
        var observed = new ConcurrentBag<byte[]>();
        await using var recovered = new RecoverableDictationSession(_store.Open(id), (wav, _) =>
        {
            observed.Add(wav.ToArray());
            return Task.FromResult("hello");
        });
        recovered.FinishCapture();
        Assert.Equal("hello", await recovered.WaitForCompletionAsync(TimeSpan.FromSeconds(5), CancellationToken.None));
        Assert.Equal(expected.Length, observed.Count);
        Assert.All(expected, chunk => Assert.Contains(observed, wav => wav.SequenceEqual(chunk.Wav)));
    }

    [Fact]
    public async Task Transient_errors_retry_but_permanent_errors_pause_and_keep_audio()
    {
        var journal = _store.Create("auto");
        var calls = 0;
        await using (var session = new RecoverableDictationSession(journal, (_, _) =>
        {
            if (Interlocked.Increment(ref calls) == 1) throw new HttpRequestException("offline");
            return Task.FromResult("recovered");
        }, retryDelay: TimeSpan.FromMilliseconds(10)))
        {
            session.Append(ChunkerTests.Pcm(100));
            session.FinishCapture();
            Assert.Equal("recovered", await session.WaitForCompletionAsync(TimeSpan.FromSeconds(5), CancellationToken.None));
            Assert.Equal(2, calls);
            Assert.Null(session.Progress.ServiceError);
        }
        var permanent = _store.Create("auto");
        calls = 0;
        await using var paused = new RecoverableDictationSession(permanent, (_, _) =>
        {
            Interlocked.Increment(ref calls);
            throw new ApiException(ApiErrorKind.Unauthorized, 401, null, "signed out");
        });
        paused.Append(ChunkerTests.Pcm(24000));
        paused.FinishCapture();
        await UntilAsync(() => paused.Progress.ServiceError is not null);
        await Task.Delay(600);
        Assert.InRange(calls, 1, 2);
        Assert.True(paused.Progress.PendingChunks > 2);
        Assert.NotEmpty(permanent.ReadAudio(0));
        Assert.Null(await paused.WaitForCompletionAsync(TimeSpan.FromMilliseconds(50), CancellationToken.None));
    }

    [Fact]
    public async Task Quota_failure_keeps_previous_checkpoint_and_is_visible()
    {
        var limited = new RecoveryStore(_root, new FakeSecretProtector(), maxBytes: 30000);
        var journal = limited.Create("auto");
        await using var session = new RecoverableDictationSession(journal, (_, _) => Task.FromResult("never"));
        session.Append(ChunkerTests.Pcm(40));
        Assert.ThrowsAny<IOException>(() => session.Append(ChunkerTests.Pcm(2000)));
        Assert.NotNull(session.Progress.Failure);
        Assert.Equal(0, limited.Open(session.Id).Snapshot.CapturedBytes);
    }

    [Fact]
    public void Preview_is_bounded_without_damaging_unicode()
    {
        var text = string.Join(' ', Enumerable.Repeat("word", 100));
        Assert.Equal("... " + string.Join(' ', Enumerable.Repeat("word", 30)), RecoverableDictationSession.PreviewTail(text));
        var emoji = string.Concat(Enumerable.Repeat("\U0001F600", 300));
        var tail = RecoverableDictationSession.PreviewTail(emoji);
        Assert.Equal(224, tail.EnumerateRunes().Count());
        Assert.DoesNotContain("\uFFFD", tail);
    }

    private static async Task UntilAsync(Func<bool> done)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (!done()) await Task.Delay(10, timeout.Token);
    }

    public void Dispose()
    {
        foreach (var item in _store.List()) _store.Delete(item.Id);
        Directory.Delete(_root);
    }
}
