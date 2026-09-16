using System.Buffers.Binary;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Text;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Infrastructure;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public sealed class RecoveryStoreTests : IDisposable
{
    private readonly string _root = Path.Combine(Directory.GetCurrentDirectory(),
        "recovery-test-" + Guid.NewGuid().ToString("N"));
    private readonly TestProtector _protector = new();
    private readonly TestClock _clock = new();

    private RecoveryStore Store(long quota = 268435456) => new(_root, _protector, _clock, quota);
    private string DirectoryFor(string id) => Path.Combine(_root, id);
    private string ManifestFor(string id) => Path.Combine(DirectoryFor(id), "manifest.enc");
    private string AudioFor(string id, int index) => Path.Combine(DirectoryFor(id), $"{index:D8}.wav.enc");
    private static AudioChunk Audio(bool overlaps = false) =>
        new(PcmChunker.ToWav(Enumerable.Repeat((byte)42, 3200).ToArray()), overlaps);

    [Fact]
    public void Routine_operations_decrypt_only_manifest_and_requested_audio()
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 64000, Enumerable.Range(0, 20).Select(_ => Audio()).ToArray());
        var before = _protector.UnprotectedOutputs.Count;
        Assert.Equal(20, journal.Snapshot.Chunks.Count);
        Assert.Equal(before + 1, _protector.UnprotectedOutputs.Count);
        before = _protector.UnprotectedOutputs.Count;
        journal.Checkpoint([1], 67200, [Audio()]);
        Assert.Equal(before + 1, _protector.UnprotectedOutputs.Count);
        before = _protector.UnprotectedOutputs.Count;
        journal.SaveResult(0, "committed");
        Assert.Equal(before + 1, _protector.UnprotectedOutputs.Count);
        before = _protector.UnprotectedOutputs.Count;
        var audio = journal.ReadAudio(1);
        try { Assert.Equal(before + 2, _protector.UnprotectedOutputs.Count); }
        finally { CryptographicOperations.ZeroMemory(audio); }
    }

    [Fact]
    public void Source_end_positions_survive_results_checkpoints_and_restart_without_double_counting_overlap()
    {
        using var journal = Store().Create("en", "bound context");
        journal.Checkpoint([1], 6000,
            [Audio() with { EndByte = 3200 }, Audio(true) with { EndByte = 5760 }]);
        journal.SaveResult(1, "second arrives first");
        Assert.Empty(journal.Snapshot.Chunks.TakeWhile(c => c.Text is not null));
        journal.Checkpoint([2], 9600, [Audio(true) with { EndByte = 8960 }], complete: true);
        journal.SaveResult(0, "first");
        using var reopened = Store().Open(journal.Snapshot.Id);
        var snapshot = reopened.Snapshot;
        Assert.Equal(new long[] { 3200, 5760, 8960 }, snapshot.Chunks.Select(c => c.EndByte));
        Assert.Equal(5760, snapshot.Chunks.TakeWhile(c => c.Text is not null).Last().EndByte);
        Assert.Equal(9600, snapshot.CapturedBytes);
        Assert.Equal(new byte[] { 2 }, snapshot.Tail);
        Assert.Equal("bound context", snapshot.Context);
        reopened.SaveResult(2, "");
        Assert.Equal(8960, reopened.Snapshot.Chunks.TakeWhile(c => c.Text is not null).Last().EndByte);
    }

    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public void Legacy_chunk_positions_remain_unknown_after_upgrading(int version)
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 3200, [Audio()]);
        var id = journal.Snapshot.Id;
        var path = ManifestFor(id);
        var plaintext = _protector.Unprotect(File.ReadAllBytes(path));
        BinaryPrimitives.WriteInt32LittleEndian(plaintext.AsSpan(4), version);
        const int endByteOffset = 129;
        var legacy = plaintext[..endByteOffset].Concat(plaintext[(endByteOffset + 8)..]).ToArray();
        if (version == 1)
        {
            var withoutContext = legacy[..^4];
            CryptographicOperations.ZeroMemory(legacy);
            legacy = withoutContext;
        }
        try { File.WriteAllBytes(path, _protector.Protect(legacy)); }
        finally
        {
            CryptographicOperations.ZeroMemory(plaintext);
            CryptographicOperations.ZeroMemory(legacy);
        }
        using var reopened = Store().Open(id);
        Assert.Equal(0, Assert.Single(reopened.Snapshot.Chunks).EndByte);
        reopened.SaveResult(0, "completed legacy chunk");
        using var upgraded = Store().Open(id);
        Assert.Equal(0, Assert.Single(upgraded.Snapshot.Chunks).EndByte);
    }

    [Theory]
    [InlineData(-1L)]
    [InlineData(9601L)]
    [InlineData(long.MaxValue)]
    public void Invalid_source_positions_are_rejected_before_checkpoint_publication_and_on_load(long endByte)
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 9600, [Audio() with { EndByte = 3200 }]);
        var id = journal.Snapshot.Id;
        var path = ManifestFor(id);
        var original = File.ReadAllBytes(path);
        Assert.Throws<IOException>(() => journal.Checkpoint([1], 9600, [Audio(true) with { EndByte = endByte }]));
        Assert.Throws<IOException>(() => journal.Checkpoint([1], 9600, [Audio(true) with { EndByte = 3199 }]));
        Assert.Equal(original, File.ReadAllBytes(path));
        Assert.False(File.Exists(AudioFor(id, 1)));
        var plaintext = _protector.Unprotect(original);
        BinaryPrimitives.WriteInt64LittleEndian(plaintext.AsSpan(129), endByte);
        try { File.WriteAllBytes(path, _protector.Protect(plaintext)); }
        finally { CryptographicOperations.ZeroMemory(plaintext); }
        Assert.Throws<IOException>(() => Store().Open(id));
        Assert.NotNull(Assert.Single(Store().List()).Error);
    }

    [Fact]
    public void Opaque_context_is_encrypted_and_survives_checkpoints_results_and_reopening()
    {
        const string context = "https://backend.example.test/private-api\naccount=uživatel-42";
        using var journal = Store().Create("cs", context);
        Assert.Equal(context, journal.Snapshot.Context);
        journal.Checkpoint([1, 2], 3200, [Audio()]);
        journal.SaveResult(0, "durable result");
        journal.Checkpoint([], 3200, [], complete: true);
        var id = journal.Snapshot.Id;
        using var reopened = Store().Open(id);
        Assert.Equal(context, reopened.Snapshot.Context);
        Assert.Equal("durable result", Assert.Single(reopened.Snapshot.Chunks).Text);
        Assert.False(File.ReadAllBytes(ManifestFor(id)).AsSpan().IndexOf(Encoding.UTF8.GetBytes(context)) >= 0);
        Assert.Null(Assert.Single(Store().List()).Error);
    }

    [Fact]
    public void Default_and_legacy_context_are_empty_and_never_inferred()
    {
        using var journal = Store().Create("en");
        Assert.Equal("", journal.Snapshot.Context);
        var id = journal.Snapshot.Id;
        var path = ManifestFor(id);
        var plaintext = _protector.Unprotect(File.ReadAllBytes(path));
        BinaryPrimitives.WriteInt32LittleEndian(plaintext.AsSpan(4), 1);
        var legacy = plaintext[..^4];
        try { File.WriteAllBytes(path, _protector.Protect(legacy)); }
        finally
        {
            CryptographicOperations.ZeroMemory(plaintext);
            CryptographicOperations.ZeroMemory(legacy);
        }
        using var reopened = Store().Open(id);
        Assert.Equal("", reopened.Snapshot.Context);
        reopened.Checkpoint([], 0, []);
        using var upgraded = Store().Open(id);
        Assert.Equal("", upgraded.Snapshot.Context);
    }

    [Fact]
    public void Context_size_is_bounded_on_creation_and_loading()
    {
        var store = Store();
        Assert.Throws<IOException>(() => store.Create("en", new string('x', 16 * 1024 + 1)));
        Assert.Empty(store.List());
        using var journal = store.Create("en");
        var id = journal.Snapshot.Id;
        var path = ManifestFor(id);
        var plaintext = _protector.Unprotect(File.ReadAllBytes(path));
        BinaryPrimitives.WriteInt32LittleEndian(plaintext.AsSpan(plaintext.Length - 4), int.MaxValue);
        try { File.WriteAllBytes(path, _protector.Protect(plaintext)); }
        finally { CryptographicOperations.ZeroMemory(plaintext); }
        Assert.Throws<IOException>(() => store.Open(id));
        Assert.NotNull(Assert.Single(store.List()).Error);
    }

    [Fact]
    public void Encrypted_roundtrip_preserves_independent_snapshots_and_removes_completed_audio()
    {
        var store = Store();
        using var journal = store.Create("cs");
        var id = journal.Snapshot.Id;
        var tail = Encoding.UTF8.GetBytes("opaque chunker state and buffered speech");
        var chunk = Audio();
        journal.Checkpoint(tail, 6400, [chunk, Audio(true)]);
        var snapshot = journal.Snapshot;
        snapshot.Tail[0] ^= 0xff;
        ((RecoveryChunk[])snapshot.Chunks)[0] = new(17, false, "local mutation");
        Assert.Equal(tail, journal.Snapshot.Tail);
        Assert.Equal(0, journal.Snapshot.Chunks[0].Index);
        var readAudio = journal.ReadAudio(0);
        Assert.Equal(chunk.Wav, readAudio);
        CryptographicOperations.ZeroMemory(readAudio);
        Assert.False(File.ReadAllBytes(AudioFor(id, 0)).AsSpan().IndexOf(chunk.Wav) >= 0);
        Assert.False(File.ReadAllBytes(ManifestFor(id)).AsSpan().IndexOf(tail) >= 0);
        journal.SaveResult(0, "private transcription text");
        journal.SaveResult(1, "");
        Assert.False(File.Exists(AudioFor(id, 0)));
        Assert.False(File.Exists(AudioFor(id, 1)));
        journal.Checkpoint([], 6400, [], complete: true);
        using var reopened = Store().Open(id);
        var recovered = reopened.Snapshot;
        Assert.Equal("private transcription text", recovered.Chunks[0].Text);
        Assert.Equal("", recovered.Chunks[1].Text);
        Assert.True(recovered.Chunks[1].OverlapsPrevious);
        Assert.True(recovered.CaptureComplete);
        Assert.Equal(6400, recovered.CapturedBytes);
        var info = Assert.Single(Store().List());
        Assert.Null(info.Error);
        Assert.Equal("cs", info.Language);
        Assert.Equal(0, info.PendingChunks);
        Assert.True(info.CaptureComplete);
        foreach (var file in Directory.GetFiles(_root, "*", SearchOption.AllDirectories))
        {
            var data = File.ReadAllBytes(file);
            Assert.False(data.AsSpan().IndexOf(Encoding.UTF8.GetBytes("private transcription text")) >= 0);
            Assert.False(data.AsSpan().IndexOf(tail) >= 0);
            Assert.False(data.AsSpan().IndexOf(chunk.Wav) >= 0);
        }
        Assert.Throws<IOException>(() => reopened.ReadAudio(0));
        Assert.All(_protector.ProtectedInputs, bytes => Assert.All(bytes, b => Assert.Equal(0, b)));
        Assert.All(_protector.UnprotectedOutputs, bytes => Assert.All(bytes, b => Assert.Equal(0, b)));
    }

    [Fact]
    public async Task Concurrent_checkpoints_and_out_of_order_results_do_not_lose_updates_across_handles()
    {
        var store = Store();
        using var journal = store.Create("en");
        journal.Checkpoint([0], 3200 * 20, Enumerable.Range(0, 20).Select(_ => Audio()).ToArray());
        using var other = Store().Open(journal.Snapshot.Id);
        using var start = new ManualResetEventSlim();
        var checkpoint = Task.Run(() =>
        {
            start.Wait();
            for (var i = 0; i < 12; i++)
                journal.Checkpoint([(byte)i], 64000 + (i + 1) * 3200L, [Audio(true)]);
        });
        var even = Task.Run(() =>
        {
            start.Wait();
            for (var i = 18; i >= 0; i -= 2) journal.SaveResult(i, $"result-{i}");
        });
        var odd = Task.Run(() =>
        {
            start.Wait();
            for (var i = 19; i >= 1; i -= 2) other.SaveResult(i, $"result-{i}");
        });
        start.Set();
        await Task.WhenAll(checkpoint, even, odd);
        using var reopened = Store().Open(journal.Snapshot.Id);
        var snapshot = reopened.Snapshot;
        Assert.Equal(32, snapshot.Chunks.Count);
        Assert.Equal(new byte[] { 11 }, snapshot.Tail);
        Assert.Equal(102400, snapshot.CapturedBytes);
        Assert.All(snapshot.Chunks.Take(20), c => Assert.Equal($"result-{c.Index}", c.Text));
        Assert.All(snapshot.Chunks.Skip(20), c => Assert.Null(c.Text));
        Assert.Equal(12, Assert.Single(store.List()).PendingChunks);
    }

    [Fact]
    public void Failed_manifest_encryption_leaves_previous_cursor_tail_and_entries_recoverable()
    {
        using var journal = Store().Create("en");
        var id = journal.Snapshot.Id;
        journal.Checkpoint([1, 2], 3200, [Audio()]);
        journal.SaveResult(0, "already durable");
        var previous = File.ReadAllBytes(ManifestFor(id));
        _protector.FailProtectOnCall = _protector.ProtectCalls + 2;
        Assert.Throws<IOException>(() => journal.Checkpoint([3, 4], 6400, [Audio(true)], true));
        Assert.Equal(previous, File.ReadAllBytes(ManifestFor(id)));
        Assert.True(File.Exists(AudioFor(id, 1)));
        using var recovered = Store().Open(id);
        Assert.Equal(new byte[] { 1, 2 }, recovered.Snapshot.Tail);
        Assert.Equal(3200, recovered.Snapshot.CapturedBytes);
        Assert.False(recovered.Snapshot.CaptureComplete);
        Assert.Equal("already durable", Assert.Single(recovered.Snapshot.Chunks).Text);
        Assert.Throws<IOException>(() => recovered.ReadAudio(1));
        recovered.Checkpoint([3, 4], 6400, [Audio(true)], true);
        Assert.Equal(2, recovered.Snapshot.Chunks.Count);
        Assert.Equal(6400, recovered.Snapshot.CapturedBytes);
        Assert.Equal(new byte[] { 3, 4 }, recovered.Snapshot.Tail);
    }

    [Fact]
    public void Failed_result_commit_keeps_audio_and_pending_result()
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 3200, [Audio()]);
        var id = journal.Snapshot.Id;
        _protector.FailProtectOnCall = _protector.ProtectCalls + 1;
        Assert.Throws<IOException>(() => journal.SaveResult(0, "not committed"));
        Assert.True(File.Exists(AudioFor(id, 0)));
        Assert.Null(Assert.Single(journal.Snapshot.Chunks).Text);
        journal.SaveResult(0, "committed");
        Assert.False(File.Exists(AudioFor(id, 0)));
    }

    [Fact]
    public void Duplicate_same_results_are_idempotent_but_conflicting_results_are_rejected()
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 6400, [Audio(), Audio(true)], true);
        journal.SaveResult(1, "second");
        journal.SaveResult(0, "");
        var original = File.ReadAllBytes(ManifestFor(journal.Snapshot.Id));
        journal.SaveResult(0, "");
        journal.SaveResult(1, "second");
        Assert.Equal(original, File.ReadAllBytes(ManifestFor(journal.Snapshot.Id)));
        Assert.Throws<IOException>(() => journal.SaveResult(0, "conflict"));
        Assert.Throws<IOException>(() => journal.SaveResult(-1, "invalid"));
        Assert.Throws<IOException>(() => journal.SaveResult(2, "invalid"));
        Assert.Equal("", journal.Snapshot.Chunks[0].Text);
        Assert.Equal("second", journal.Snapshot.Chunks[1].Text);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Corrupt_entries_remain_visible_open_fails_and_explicit_delete_preserves_other_sessions(bool audio)
    {
        var store = Store();
        using var bad = store.Create("en");
        using var good = store.Create("cs");
        bad.Checkpoint([], 3200, [Audio()]);
        var id = bad.Snapshot.Id;
        var path = audio ? AudioFor(id, 0) : ManifestFor(id);
        var corrupted = File.ReadAllBytes(path);
        corrupted[^1] ^= 0xff;
        File.WriteAllBytes(path, corrupted);
        var list = store.List();
        Assert.Equal(2, list.Count);
        Assert.Equal("Recovery data is unreadable or corrupt.", list.Single(x => x.Id == id).Error);
        Assert.Null(list.Single(x => x.Id == good.Snapshot.Id).Error);
        Assert.Throws<IOException>(() => store.Open(id));
        if (!audio)
        {
            Assert.Throws<IOException>(() => bad.Checkpoint([], 3200, []));
            Assert.Throws<IOException>(() => bad.SaveResult(0, "no overwrite"));
            Assert.Equal(corrupted, File.ReadAllBytes(path));
        }
        store.Delete(id);
        Assert.Single(store.List());
        Assert.True(Directory.Exists(DirectoryFor(good.Snapshot.Id)));
    }

    [Theory]
    [InlineData("version")]
    [InlineData("count")]
    [InlineData("tail")]
    [InlineData("index")]
    [InlineData("duration")]
    [InlineData("captured")]
    public void Authenticated_but_invalid_manifest_fields_are_bounded_and_rejected(string field)
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 3200, [Audio()]);
        var id = journal.Snapshot.Id;
        var path = ManifestFor(id);
        var bytes = _protector.Unprotect(File.ReadAllBytes(path));
        // Header: magic/version, 32-byte ID, two timestamps, two-byte language, capture position.
        const int capturedOffset = 66;
        const int tailOffset = 74;
        const int countOffset = 79;
        const int indexOffset = 83;
        var offset = field switch
        {
            "version" => 4,
            "count" => countOffset,
            "tail" => tailOffset,
            "index" => indexOffset,
            "duration" => indexOffset + 6,
            _ => capturedOffset
        };
        BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(offset), int.MaxValue);
        if (field == "captured")
            BinaryPrimitives.WriteInt64LittleEndian(bytes.AsSpan(offset), long.MaxValue);
        File.WriteAllBytes(path, _protector.Protect(bytes));
        CryptographicOperations.ZeroMemory(bytes);
        Assert.Throws<IOException>(() => Store().Open(id));
        Assert.NotNull(Assert.Single(Store().List()).Error);
    }

    [Fact]
    public void Missing_audio_and_swapped_valid_audio_fail_explicitly()
    {
        using var journal = Store().Create("en");
        journal.Checkpoint([], 3200, [Audio()]);
        var id = journal.Snapshot.Id;
        var different = PcmChunker.ToWav(Enumerable.Repeat((byte)7, 3200).ToArray());
        File.WriteAllBytes(AudioFor(id, 0), _protector.Protect(different));
        Assert.Throws<IOException>(() => journal.ReadAudio(0));
        Assert.NotNull(Assert.Single(Store().List()).Error);
        File.Delete(AudioFor(id, 0));
        Assert.Throws<IOException>(() => Store().Open(id));
    }

    [Fact]
    public void Quota_counts_all_sessions_orphans_and_atomic_replacement_peak()
    {
        var store = Store();
        using var first = store.Create("en");
        using var second = store.Create("en");
        var id = first.Snapshot.Id;
        var original = File.ReadAllBytes(ManifestFor(id));
        File.WriteAllBytes(Path.Combine(_root, "unpublished-orphan"), new byte[4096]);
        var used = Directory.GetFiles(_root, "*", SearchOption.AllDirectories)
            .Sum(path => new FileInfo(path).Length);
        using var constrained = Store(used).Open(id);
        var failure = Assert.Throws<IOException>(() => constrained.Checkpoint([1], 0, []));
        Assert.Contains("quota", failure.Message, StringComparison.OrdinalIgnoreCase);
        Assert.Equal(original, File.ReadAllBytes(ManifestFor(id)));
        Assert.Empty(Directory.GetFiles(_root, "*.tmp", SearchOption.AllDirectories));
        Assert.Equal(2, Store().List().Count);
    }

    [Fact]
    public void Invalid_audio_tail_positions_results_and_oversized_ciphertext_are_rejected()
    {
        using var journal = Store().Create("en");
        var id = journal.Snapshot.Id;
        Assert.Throws<IOException>(() => journal.Checkpoint(new byte[1024 * 1024 + 1], 0, []));
        Assert.Throws<IOException>(() => journal.Checkpoint([], -1, []));
        Assert.Throws<IOException>(() => journal.Checkpoint([], long.MaxValue, []));
        Assert.Throws<IOException>(() => journal.Checkpoint([], 3200, [new([1, 2], false)]));
        var tooLong = PcmChunker.ToWav(new byte[PcmChunker.BytesPerSecond * 7]);
        Assert.Throws<IOException>(() => journal.Checkpoint([], 0, [new(tooLong, false)]));
        journal.Checkpoint([], 3200, [Audio()]);
        Assert.Throws<IOException>(() => journal.Checkpoint([], 3199, []));
        Assert.Throws<IOException>(() => journal.SaveResult(0, new string('a', 1024 * 1024 + 1)));
        Assert.Null(journal.Snapshot.Chunks[0].Text);
        using (var file = File.OpenWrite(ManifestFor(id))) file.SetLength(17 * 1024 * 1024);
        Assert.Throws<IOException>(() => Store().Open(id));
        Assert.NotNull(Assert.Single(Store().List()).Error);
    }

    [Theory]
    [InlineData("../elsewhere")]
    [InlineData("..\\elsewhere")]
    [InlineData("C:\\elsewhere")]
    [InlineData("00000000-0000-0000-0000-000000000000")]
    [InlineData("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\..")]
    [InlineData("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
    [InlineData("")]
    public void Traversal_and_noncanonical_ids_are_rejected(string id)
    {
        var store = Store();
        Assert.Throws<ArgumentException>(() => store.Open(id));
        Assert.Throws<ArgumentException>(() => store.Delete(id));
    }

    [Fact]
    public void Cleanup_uses_last_update_skips_live_and_corrupt_entries_and_ignores_unknown_directories()
    {
        var store = Store();
        using var expired = store.Create("en");
        using var updated = store.Create("cs");
        using var active = store.Create("en");
        using var corrupt = store.Create("en");
        var expiredId = expired.Snapshot.Id;
        var updatedId = updated.Snapshot.Id;
        var activeId = active.Snapshot.Id;
        var corruptId = corrupt.Snapshot.Id;
        expired.Dispose();
        _clock.UtcNow += TimeSpan.FromHours(47);
        updated.Checkpoint([1], 0, []);
        updated.Dispose();
        corrupt.Dispose();
        File.WriteAllBytes(ManifestFor(corruptId), [1, 2, 3]);
        var foreign = Path.Combine(_root, "not-a-session");
        Directory.CreateDirectory(foreign);
        File.WriteAllText(Path.Combine(foreign, "preserve"), "unrelated");
        _clock.UtcNow += TimeSpan.FromHours(2);
        store.Cleanup();
        Assert.False(Directory.Exists(DirectoryFor(expiredId)));
        Assert.True(Directory.Exists(DirectoryFor(updatedId)));
        Assert.True(Directory.Exists(DirectoryFor(activeId)));
        Assert.True(Directory.Exists(DirectoryFor(corruptId)));
        Assert.True(Directory.Exists(foreign));
        active.Dispose();
        store.Cleanup();
        Assert.False(Directory.Exists(DirectoryFor(activeId)));
    }

    [Fact]
    public void Delete_is_exact_idempotent_and_rejects_unexpected_nested_directories()
    {
        var store = Store();
        using var journal = store.Create("en");
        using var survivor = store.Create("cs");
        var id = journal.Snapshot.Id;
        var nested = Path.Combine(DirectoryFor(id), "unexpected");
        Directory.CreateDirectory(nested);
        Assert.Throws<IOException>(() => journal.Delete());
        Directory.Delete(nested);
        journal.Delete();
        journal.Delete();
        Assert.ThrowsAny<IOException>(() => journal.Checkpoint([], 0, []));
        Assert.Equal(survivor.Snapshot.Id, Assert.Single(store.List()).Id);
    }

    [Fact]
    public void File_reparse_points_cannot_escape_session_on_read_write_or_delete()
    {
        var store = Store();
        using var journal = store.Create("en");
        var id = journal.Snapshot.Id;
        var original = File.ReadAllBytes(ManifestFor(id));
        var target = Path.Combine(_root, "outside-session");
        File.WriteAllBytes(target, original);
        File.Delete(ManifestFor(id));
        try
        {
            File.CreateSymbolicLink(ManifestFor(id), target);
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException ||
            (ex is IOException && (ex.HResult & 0xffff) == 1314))
        {
            // Windows without Developer Mode cannot create symbolic links without elevation.
            File.WriteAllBytes(ManifestFor(id), original);
            return;
        }
        try
        {
            Assert.Throws<IOException>(() => store.Open(id));
            Assert.Throws<IOException>(() => journal.Checkpoint([], 0, []));
            Assert.Throws<IOException>(() => store.Delete(id));
            Assert.NotNull(Assert.Single(store.List()).Error);
            Assert.Equal(original, File.ReadAllBytes(target));
        }
        finally
        {
            File.Delete(ManifestFor(id));
            File.WriteAllBytes(ManifestFor(id), original);
        }
    }

    [Fact]
    public void Windows_directory_junctions_are_rejected_without_following_their_targets()
    {
        if (!OperatingSystem.IsWindows()) return;
        var store = Store();
        using var journal = store.Create("en");
        var id = journal.Snapshot.Id;
        var target = Path.Combine(_root, "junction-target");
        Directory.Move(DirectoryFor(id), target);
        var start = new ProcessStartInfo(Environment.GetEnvironmentVariable("ComSpec") ?? "cmd.exe")
        {
            Arguments = $"/d /c mklink /J \"{DirectoryFor(id)}\" \"{target}\"",
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEnd() + process.StandardError.ReadToEnd();
        Assert.True(process.WaitForExit(10000), "Junction creation timed out.");
        Assert.True(process.ExitCode == 0, output);
        try
        {
            Assert.Throws<IOException>(() => store.Open(id));
            Assert.Throws<IOException>(() => store.Delete(id));
            Assert.Throws<IOException>(() => journal.Checkpoint([], 0, []));
            Assert.Throws<IOException>(() => new RecoveryStore(DirectoryFor(id), _protector));
            Assert.NotNull(Assert.Single(store.List()).Error);
            Assert.True(File.Exists(Path.Combine(target, "manifest.enc")));
        }
        finally { Directory.Delete(DirectoryFor(id)); }
    }

    public void Dispose()
    {
        if (Directory.Exists(_root)) Directory.Delete(_root, recursive: true);
    }

    private sealed class TestClock : IClock
    {
        public DateTimeOffset UtcNow { get; set; } = new(2026, 9, 16, 6, 0, 0, TimeSpan.Zero);
    }

    private sealed class TestProtector : ISecretProtector
    {
        private readonly byte[] _key = SHA256.HashData("test-only-recovery-key"u8);
        public List<byte[]> ProtectedInputs { get; } = [];
        public List<byte[]> UnprotectedOutputs { get; } = [];
        public int ProtectCalls { get; private set; }
        public int FailProtectOnCall { get; set; }

        public byte[] Protect(byte[] plaintext)
        {
            ProtectedInputs.Add(plaintext);
            if (++ProtectCalls == FailProtectOnCall) throw new CryptographicException("Injected encryption failure.");
            var result = new byte[plaintext.Length + 28];
            RandomNumberGenerator.Fill(result.AsSpan(0, 12));
            using var aes = new AesGcm(_key, 16);
            aes.Encrypt(result.AsSpan(0, 12), plaintext, result.AsSpan(28), result.AsSpan(12, 16));
            return result;
        }

        public byte[] Unprotect(byte[] ciphertext)
        {
            if (ciphertext.Length < 28) throw new CryptographicException("Invalid ciphertext.");
            var plaintext = new byte[ciphertext.Length - 28];
            using var aes = new AesGcm(_key, 16);
            aes.Decrypt(ciphertext.AsSpan(0, 12), ciphertext.AsSpan(28), ciphertext.AsSpan(12, 16), plaintext);
            UnprotectedOutputs.Add(plaintext);
            return plaintext;
        }
    }
}
