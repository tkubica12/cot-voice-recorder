using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Threading.Channels;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public sealed class StreamingDictationTests : IDisposable
{
    private readonly string _root = Path.Combine(Directory.GetCurrentDirectory(), "stream-test-" + Guid.NewGuid().ToString("N"));
    private readonly Protector _protector = new();
    private readonly Clock _clock = new();
    private RecoveryStore Store => new(Path.Combine(_root, "recovery"), _protector, _clock);
    private const string Context = "https://backend.example.test\nowner@example.test";

    private static byte[] Audio(double seconds, short value = 2000)
    {
        var pcm = new byte[(int)(seconds * PcmChunker.BytesPerSecond)];
        for (var i = 0; i < pcm.Length; i += 2) BinaryPrimitives.WriteInt16LittleEndian(pcm.AsSpan(i), value);
        return pcm;
    }

    private static async Task Until(Func<bool> condition)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(6));
        while (!condition()) await Task.Delay(10, timeout.Token);
    }

    [Fact]
    public async Task Partial_suffix_is_replaced_and_never_delivered_as_a_final()
    {
        var journal = Store.Create("cs", Context, streaming: true);
        var factory = new Factory { DelayCompletion = true };
        await using var session = new StreamingDictationSession(journal, factory, (_, _) => throw new Exception("No fallback"));
        session.Append(Audio(2));
        await Until(() => session.Progress.Preview.Contains("hello new"));
        Assert.DoesNotContain("old", session.Progress.Preview);
        Assert.Equal("", session.Progress.Transcript);
        session.FinishCapture();
        Assert.Null(await session.WaitForCompletionAsync(TimeSpan.FromMilliseconds(100), CancellationToken.None));
        await Until(() => factory.Connections[0].Finished);
        factory.Connections[0].Complete();
        Assert.Equal("hello final", await session.WaitForCompletionAsync(TimeSpan.FromSeconds(3), CancellationToken.None));
        Assert.Equal(64000, journal.Snapshot.ConfirmedBytes);
        Assert.Equal(19200, journal.Snapshot.Tail.Length);
        Assert.True(journal.Snapshot.UsedStreaming);
        Assert.All(journal.Snapshot.Chunks, c => Assert.Equal("", c.Text));
    }

    [Fact]
    public async Task Reconnect_replays_only_unconfirmed_audio_without_duplicate_prefix()
    {
        var journal = Store.Create("auto", Context, streaming: true);
        var factory = new Factory { DropFirstAfter = 85000 };
        await using var session = new StreamingDictationSession(journal, factory, (_, _) => throw new Exception("No fallback"));
        var pcm = Audio(2).Concat(Audio(0.5, 0)).Concat(Audio(2)).ToArray();
        session.Append(pcm);
        session.FinishCapture();
        var text = await session.WaitForCompletionAsync(TimeSpan.FromSeconds(6), CancellationToken.None);
        Assert.NotNull(text);
        Assert.Equal(2, factory.Connections.Count);
        var second = factory.Connections[1];
        Assert.True(second.Start > 0);
        Assert.Equal(pcm.AsSpan((int)second.Start).ToArray(), second.Audio.ToArray());
        Assert.Equal(pcm.Length, journal.Snapshot.ConfirmedBytes);
        Assert.Equal("hello final hello final", text);
    }

    [Fact]
    public async Task Offline_audio_survives_disposal_and_recovers_with_existing_wav_transcriber()
    {
        var store = Store;
        var journal = store.Create("cs", Context, streaming: true);
        var id = journal.Snapshot.Id;
        var pcm = Audio(8);
        await using (var session = new StreamingDictationSession(journal, new Factory { Offline = true },
            (_, _) => throw new Exception("Live capture must not discard backlog")))
        {
            session.Append(pcm);
            session.FinishCapture();
            Assert.Null(await session.WaitForCompletionAsync(TimeSpan.FromMilliseconds(100), CancellationToken.None));
            Assert.Equal(4, session.Progress.PendingChunks);
        }
        using var reopened = store.Open(id);
        var calls = 0;
        await using var recovery = new StreamingDictationSession(reopened, null,
            (_, _) => Task.FromResult(++calls == 1 ? "hello unique" : "unique ending"), recovery: true);
        Assert.Equal("hello unique ending",
            await recovery.WaitForCompletionAsync(TimeSpan.FromSeconds(3), CancellationToken.None));
        Assert.Equal(2, calls);
        Assert.True(reopened.Snapshot.UsedFallback);
        Assert.False(reopened.Snapshot.UsedStreaming);
        Assert.Equal(pcm.Length, reopened.Snapshot.ConfirmedBytes);
    }

    [Fact]
    public async Task Partial_checkpoint_confirmation_retains_exact_audio_context_across_restart()
    {
        var journal = Store.Create("cs", Context, streaming: true);
        var pcm = Audio(2);
        journal.Checkpoint([], pcm.Length, [new(PcmChunker.ToWav(pcm), false, pcm.Length)], true);
        journal.SaveStreamingResult(32000, "one", pcm.AsSpan(12800, 19200).ToArray());
        var id = journal.Snapshot.Id;
        journal.Dispose();
        using var reopened = Store.Open(id);
        byte[]? submitted = null;
        await using var session = new StreamingDictationSession(reopened, null, (wav, _) =>
        {
            submitted = wav.AsSpan(44).ToArray();
            return Task.FromResult("one two");
        }, recovery: true);
        Assert.Equal("one two", await session.WaitForCompletionAsync(TimeSpan.FromSeconds(3), CancellationToken.None));
        Assert.Equal(pcm.AsSpan(12800).ToArray(), submitted);
        Assert.Equal(pcm.Length, reopened.Snapshot.ConfirmedBytes);
    }

    [Fact]
    public void Failed_confirmation_never_deletes_unconfirmed_audio()
    {
        using var journal = Store.Create("cs", Context, streaming: true);
        var pcm = Audio(2);
        journal.Checkpoint([], pcm.Length, [new(PcmChunker.ToWav(pcm), false, pcm.Length)], true);
        _protector.Fail = true;
        Assert.Throws<IOException>(() => journal.SaveStreamingResult(pcm.Length, "final"));
        _protector.Fail = false;
        Assert.Equal(0, journal.Snapshot.ConfirmedBytes);
        Assert.Equal(pcm, journal.ReadAudio(0).AsSpan(44).ToArray());
        Assert.Throws<IOException>(() => journal.SaveStreamingResult(pcm.Length + 2, "outside"));
        Assert.Throws<IOException>(() => journal.PrepareAudit("incomplete", _clock.UtcNow, false));
    }

    [Fact]
    public async Task Durable_audit_outbox_retries_without_extending_completion_time_or_copying_text()
    {
        using var journal = Completed();
        var id = journal.Snapshot.Id;
        var completed = _clock.UtcNow;
        var history = History();
        var attempts = 0;
        var errors = 0;
        var outbox = new DictationAuditOutbox(Store, history, _clock, () => Context, () => true,
            (state, _) =>
            {
                Assert.Equal(completed, state.AuditCompletedAt);
                if (++attempts == 1) throw new HttpRequestException("Offline");
                return Task.CompletedTask;
            }, NullLog.Instance, () => errors++);
        await outbox.DrainAsync(CancellationToken.None);
        Assert.True(Assert.Single(Store.List()).AuditPending);
        Assert.NotNull(history.Get(Guid.ParseExact(id, "N").ToString("D")));
        _clock.UtcNow += TimeSpan.FromHours(1);
        await outbox.DrainAsync(CancellationToken.None);
        Assert.Empty(Store.List());
        Assert.Equal(2, attempts);
        Assert.Equal(1, errors);
        Assert.Equal(completed, history.Latest()!.CompletedAt);
    }

    [Fact]
    public async Task Audit_does_not_cross_account_or_backend_boundaries()
    {
        using var journal = Completed();
        var sent = false;
        var outbox = new DictationAuditOutbox(Store, History(), _clock, () => Context + "-other",
            () => true, (_, _) => { sent = true; return Task.CompletedTask; }, NullLog.Instance);
        await outbox.DrainAsync(CancellationToken.None);
        Assert.False(sent);
        Assert.Single(Store.List());
    }

    [Fact]
    public void Frozen_audit_cannot_be_rewritten_or_retimed()
    {
        using var journal = Completed();
        Assert.Throws<IOException>(() => journal.PrepareAudit("changed", _clock.UtcNow, false));
        Assert.Throws<IOException>(() => journal.PrepareAudit("clean", _clock.UtcNow.AddSeconds(1), true));
        Assert.Equal("clean", journal.Snapshot.AuditText);
    }

    private HistoryStore History() => new(Path.Combine(_root, "history.json"), PhysicalFileSystem.Instance, _clock);

    private RecoveryJournal Completed()
    {
        var journal = Store.Create("cs", Context, streaming: true);
        var pcm = Audio(2);
        journal.Checkpoint([], pcm.Length, [new(PcmChunker.ToWav(pcm), false, pcm.Length)], true);
        journal.SaveStreamingResult(pcm.Length, "original");
        journal.PrepareAudit("clean", _clock.UtcNow, true);
        return journal;
    }

    private sealed class Clock : IClock
    {
        public DateTimeOffset UtcNow { get; set; } = DateTimeOffset.UtcNow;
    }

    private sealed class Protector : ISecretProtector
    {
        internal bool Fail;
        public byte[] Protect(byte[] value) => Fail ? throw new CryptographicException("Injected failure")
            : value.Select(b => (byte)(b ^ 0xa5)).ToArray();
        public byte[] Unprotect(byte[] value) => value.Select(b => (byte)(b ^ 0xa5)).ToArray();
    }

    private sealed class Factory : IDictationStreamFactory
    {
        internal bool DelayCompletion;
        internal bool Offline;
        internal int DropFirstAfter;
        private readonly List<Stream> _connections = [];
        internal IReadOnlyList<Stream> Connections { get { lock (_connections) return _connections.ToArray(); } }
        public Task<IDictationStream> ConnectAsync(string language, long startByte, CancellationToken ct)
        {
            if (Offline) throw new HttpRequestException("Injected outage");
            lock (_connections)
            {
                var stream = new Stream(startByte, DelayCompletion, _connections.Count == 0 ? DropFirstAfter : 0);
                _connections.Add(stream);
                return Task.FromResult<IDictationStream>(stream);
            }
        }
    }

    private sealed class Stream(long start, bool delayed, int drop) : IDictationStream
    {
        private readonly Channel<DictationStreamMessage> _events = Channel.CreateUnbounded<DictationStreamMessage>();
        internal readonly List<byte> Audio = [];
        internal long Start => start;
        internal volatile bool Finished;
        private int _item;
        private bool _preview;
        public async Task SendAudioAsync(ReadOnlyMemory<byte> pcm, CancellationToken ct)
        {
            await Task.Yield();
            if (drop > 0 && Audio.Count >= drop) throw new HttpRequestException("Injected disconnect");
            Audio.AddRange(pcm.ToArray());
            if (!_preview)
            {
                _preview = true;
                _events.Writer.TryWrite(new("delta", "item-0", "hello "));
                _events.Writer.TryWrite(new("intermediate", "item-0", "old"));
                _events.Writer.TryWrite(new("intermediate", "item-0", "new"));
            }
        }
        public Task CommitAsync(bool finish, CancellationToken ct)
        {
            Finished = finish;
            if (!delayed) Complete();
            return Task.CompletedTask;
        }
        internal void Complete()
        {
            _events.Writer.TryWrite(new("confirmed", "item-" + _item++, "hello final", start + Audio.Count));
            if (Finished) _events.Writer.TryWrite(new("done", ByteEnd: start + Audio.Count));
        }
        public async Task<DictationStreamMessage> ReceiveAsync(CancellationToken ct) =>
            await _events.Reader.ReadAsync(ct);
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    public void Dispose()
    {
        if (Directory.Exists(_root)) Directory.Delete(_root, true);
    }
}
