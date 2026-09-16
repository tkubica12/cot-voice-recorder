using System.Buffers.Binary;
using System.Collections.Concurrent;
using System.Security.Cryptography;
using System.Text;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Dictation;

public sealed record RecoveryInfo(string Id, DateTimeOffset CreatedAt, DateTimeOffset UpdatedAt,
    string Language, long CapturedBytes, int PendingChunks, bool CaptureComplete, string? Error);

/// <param name="EndByte">Absolute source PCM end position; zero means unknown for legacy chunks.</param>
public sealed record RecoveryChunk(int Index, bool OverlapsPrevious, string? Text, long EndByte = 0);

public sealed record RecoveryState(string Id, DateTimeOffset CreatedAt, DateTimeOffset UpdatedAt,
    string Language, long CapturedBytes, byte[] Tail, bool CaptureComplete,
    IReadOnlyList<RecoveryChunk> Chunks, string Context = "");

/// <summary>
/// Encrypted, crash-recoverable dictation storage. All processes writing a root must belong to
/// the same application instance. The quota includes unpublished files and atomic-write staging.
/// </summary>
public sealed class RecoveryStore
{
    internal const int MaxTailBytes = 1024 * 1024;
    internal const int MaxManifestBytes = 16 * 1024 * 1024;
    internal const int MaxTextBytes = 1024 * 1024;
    internal const int MaxContextBytes = 16 * 1024;
    internal const int MaxChunks = 100000;
    internal const int MaxAudioBytes = 44 + PcmChunker.BytesPerSecond * PcmChunker.MaxChunkMilliseconds / 1000;
    private const long MaxCapturedBytes = 48L * 60 * 60 * PcmChunker.BytesPerSecond;
    private const int EncryptionAllowance = 64 * 1024;
    private const string ManifestName = "manifest.enc";
    private static readonly UTF8Encoding Utf8 = new(false, true);
    private static readonly ConcurrentDictionary<string, RootLock> Roots = new(StringComparer.OrdinalIgnoreCase);
    private readonly string _root;
    private readonly ISecretProtector _protector;
    private readonly IClock _clock;
    private readonly long _maxBytes;
    private readonly RootLock _shared;

    private sealed class RootLock
    {
        internal readonly object Gate = new();
        internal readonly List<WeakReference<RecoveryJournal>> Journals = [];
    }

    internal sealed record StoredChunk(RecoveryChunk Chunk, int AudioBytes, byte[] Hash);
    internal sealed record Manifest(RecoveryState State, List<StoredChunk> Stored);

    public RecoveryStore(string root, ISecretProtector protector, IClock? clock = null, long maxBytes = 268435456)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(root);
        ArgumentNullException.ThrowIfNull(protector);
        if (maxBytes <= 0) throw new ArgumentOutOfRangeException(nameof(maxBytes));
        _root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
        _protector = protector;
        _clock = clock ?? SystemClock.Instance;
        _maxBytes = maxBytes;
        _shared = Roots.GetOrAdd(_root, _ => new RootLock());
        lock (_shared.Gate)
        {
            GuardPath(_root);
            Directory.CreateDirectory(_root);
            GuardPath(_root);
        }
    }

    public RecoveryJournal Create(string language, string context = "")
    {
        ValidateLanguage(language);
        ArgumentNullException.ThrowIfNull(context);
        if (Utf8.GetByteCount(context) > MaxContextBytes)
            throw new IOException("Recovery context exceeds supported bounds.");
        lock (_shared.Gate)
        {
            GuardPath(_root);
            var id = Guid.NewGuid().ToString("N");
            var directory = SessionPath(id);
            Directory.CreateDirectory(directory);
            var now = _clock.UtcNow;
            var state = new RecoveryState(id, now, now, language, 0, [], false, [], context);
            WriteManifest(new Manifest(state, []));
            return Track(id);
        }
    }

    public RecoveryJournal Open(string id)
    {
        lock (_shared.Gate)
        {
            var manifest = Load(id, inspectAudio: true);
            Clear(manifest);
            return Track(id);
        }
    }

    public IReadOnlyList<RecoveryInfo> List()
    {
        lock (_shared.Gate)
        {
            GuardPath(_root);
            var result = new List<RecoveryInfo>();
            foreach (var directory in Directory.EnumerateDirectories(_root))
            {
                var id = Path.GetFileName(directory);
                if (!ValidId(id)) continue;
                try
                {
                    var manifest = Load(id, inspectAudio: true);
                    try
                    {
                        var s = manifest.State;
                        result.Add(new(s.Id, s.CreatedAt, s.UpdatedAt, s.Language, s.CapturedBytes,
                            s.Chunks.Count(c => c.Text is null), s.CaptureComplete, null));
                    }
                    finally { Clear(manifest); }
                }
                catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
                {
                    result.Add(new(id, default, default, "", 0, 0, false,
                        "Recovery data is unreadable or corrupt."));
                }
            }
            return result.AsReadOnly();
        }
    }

    public void Delete(string id)
    {
        lock (_shared.Gate)
        {
            var directory = SessionPath(id);
            GuardPath(directory);
            if (!Directory.Exists(directory)) return;
            // Our layout is flat. Refuse unexpected directories rather than recursively following them.
            if (Directory.EnumerateDirectories(directory).Any())
                throw new IOException("Unexpected recovery directory layout.");
            var files = Directory.GetFiles(directory);
            foreach (var file in files) GuardPath(file);
            foreach (var file in files)
            {
                GuardPath(file);
                File.Delete(file);
            }
            GuardPath(directory);
            Directory.Delete(directory);
        }
    }

    /// <summary>Retains unreadable entries for explicit deletion and skips live, undisposed journals.</summary>
    public void Cleanup()
    {
        lock (_shared.Gate)
        {
            _shared.Journals.RemoveAll(w => !w.TryGetTarget(out var j) || j.IsDisposed);
            var live = _shared.Journals.Select(w => w.TryGetTarget(out var j) ? j.Id : null).ToHashSet();
            foreach (var info in List())
                if (info.Error is null && !live.Contains(info.Id) &&
                    _clock.UtcNow - info.UpdatedAt > TimeSpan.FromHours(48))
                    Delete(info.Id);
        }
    }

    private RecoveryJournal Track(string id)
    {
        var journal = new RecoveryJournal(this, id);
        _shared.Journals.RemoveAll(w => !w.TryGetTarget(out var j) || j.IsDisposed);
        _shared.Journals.Add(new(journal));
        return journal;
    }

    internal RecoveryState Snapshot(RecoveryJournal journal)
    {
        lock (_shared.Gate)
        {
            journal.EnsureOpen();
            // Loading under the shared lock also prevents stale handles from overwriting another Open().
            return Load(journal.Id).State;
        }
    }

    internal void Checkpoint(RecoveryJournal journal, byte[] tail, long capturedBytes,
        IReadOnlyList<AudioChunk> added, bool complete)
    {
        ArgumentNullException.ThrowIfNull(tail);
        ArgumentNullException.ThrowIfNull(added);
        lock (_shared.Gate)
        {
            journal.EnsureOpen();
            var current = Load(journal.Id);
            byte[]? newTail = null;
            try
            {
                if (tail.Length > MaxTailBytes || capturedBytes < current.State.CapturedBytes ||
                    capturedBytes > MaxCapturedBytes || added.Count > MaxChunks - current.Stored.Count)
                    throw new IOException("Recovery checkpoint exceeds supported bounds.");
                if (current.State.CaptureComplete)
                    throw new IOException("Recovery capture is already complete.");
                var previousEnd = current.Stored.LastOrDefault(c => c.Chunk.EndByte != 0)?.Chunk.EndByte ?? 0;
                foreach (var audio in added)
                {
                    ValidateAudio(audio.Wav);
                    previousEnd = ValidateEndByte(audio.EndByte, capturedBytes, previousEnd);
                }
                newTail = tail.ToArray();
                var stored = new List<StoredChunk>(current.Stored);
                foreach (var audio in added)
                {
                    var index = stored.Count;
                    var path = AudioPath(journal.Id, index);
                    // A previous interrupted checkpoint may have staged this unpublished index.
                    GuardPath(path);
                    if (File.Exists(path)) File.Delete(path);
                    WriteEncrypted(path, audio.Wav, MaxAudioBytes);
                    stored.Add(new(new(index, audio.OverlapsPrevious, null, audio.EndByte), audio.Wav.Length,
                        SHA256.HashData(audio.Wav)));
                }
                var next = current.State with
                {
                    UpdatedAt = UpdatedAt(current.State),
                    Tail = newTail,
                    CapturedBytes = capturedBytes,
                    CaptureComplete = complete,
                    Chunks = stored.Select(c => c.Chunk).ToArray()
                };
                WriteManifest(new(next, stored));
            }
            finally
            {
                Clear(current);
                if (newTail is not null) CryptographicOperations.ZeroMemory(newTail);
            }
        }
    }

    internal void SaveResult(RecoveryJournal journal, int index, string text)
    {
        ArgumentNullException.ThrowIfNull(text);
        lock (_shared.Gate)
        {
            journal.EnsureOpen();
            var current = Load(journal.Id);
            try
            {
                if (Utf8.GetByteCount(text) > MaxTextBytes)
                    throw new IOException("Recovery result exceeds supported bounds.");
                var entry = Entry(current, index);
                if (entry.Chunk.Text is not null)
                {
                    if (!StringComparer.Ordinal.Equals(entry.Chunk.Text, text))
                        throw new IOException("A different result is already committed for this chunk.");
                }
                else
                {
                    current.Stored[index] = entry with { Chunk = entry.Chunk with { Text = text } };
                    var next = current.State with
                    {
                        UpdatedAt = UpdatedAt(current.State),
                        Chunks = current.Stored.Select(c => c.Chunk).ToArray()
                    };
                    WriteManifest(new(next, current.Stored));
                }
                // Publication comes first: a crash may leave encrypted audio, never a pending missing file.
                var path = AudioPath(journal.Id, index);
                GuardPath(path);
                File.Delete(path);
            }
            finally { Clear(current); }
        }
    }

    internal byte[] ReadAudio(RecoveryJournal journal, int index)
    {
        lock (_shared.Gate)
        {
            journal.EnsureOpen();
            var manifest = Load(journal.Id);
            byte[]? audio = null;
            try
            {
                var entry = Entry(manifest, index);
                if (entry.Chunk.Text is not null) throw new IOException("This chunk is already transcribed.");
                audio = ReadEncrypted(AudioPath(journal.Id, index), MaxAudioBytes);
                ValidateAudio(audio);
                if (audio.Length != entry.AudioBytes ||
                    !CryptographicOperations.FixedTimeEquals(SHA256.HashData(audio), entry.Hash))
                    throw new IOException("Recovery audio does not match its manifest.");
                var result = audio;
                audio = null;
                return result;
            }
            finally
            {
                Clear(manifest);
                if (audio is not null) CryptographicOperations.ZeroMemory(audio);
            }
        }
    }

    private DateTimeOffset UpdatedAt(RecoveryState state) =>
        _clock.UtcNow > state.UpdatedAt ? _clock.UtcNow : state.UpdatedAt;

    private static StoredChunk Entry(Manifest manifest, int index) =>
        index >= 0 && index < manifest.Stored.Count
            ? manifest.Stored[index] : throw new IOException("Unknown recovery chunk.");

    private Manifest Load(string id, bool inspectAudio = false)
    {
        var bytes = ReadEncrypted(Path.Combine(SessionPath(id), ManifestName), MaxManifestBytes);
        byte[]? tail = null;
        try
        {
            using var stream = new MemoryStream(bytes, false);
            using var reader = new BinaryReader(stream, Utf8);
            if (reader.ReadInt32() != 0x5650524A)
                throw new IOException("Unsupported or corrupt recovery manifest.");
            var version = reader.ReadInt32();
            if (version is not (1 or 2 or 3) || ReadString(reader, 32) != id)
                throw new IOException("Unsupported or corrupt recovery manifest.");
            var created = new DateTimeOffset(reader.ReadInt64(), TimeSpan.Zero);
            var updated = new DateTimeOffset(reader.ReadInt64(), TimeSpan.Zero);
            if (updated < created) throw new IOException("Invalid recovery timestamps.");
            var language = ReadString(reader, 128);
            ValidateLanguage(language);
            var captured = reader.ReadInt64();
            if (captured < 0 || captured > MaxCapturedBytes)
                throw new IOException("Invalid recovery capture position.");
            tail = ReadBytes(reader, MaxTailBytes);
            var complete = ReadBoolean(reader);
            var count = reader.ReadInt32();
            var minimumEntryBytes = version >= 3 ? 54 : 46;
            if (count < 0 || count > MaxChunks || count > (stream.Length - stream.Position) / minimumEntryBytes)
                throw new IOException("Invalid recovery chunk count.");
            var stored = new List<StoredChunk>(count);
            long previousEnd = 0;
            for (var i = 0; i < count; i++)
            {
                var index = reader.ReadInt32();
                if (index != i) throw new IOException("Invalid recovery chunk index.");
                var overlaps = ReadBoolean(reader);
                var text = ReadBoolean(reader) ? ReadString(reader, MaxTextBytes) : null;
                var length = reader.ReadInt32();
                if (length < 46 || length > MaxAudioBytes || (length - 44) % 2 != 0)
                    throw new IOException("Invalid recovery audio size.");
                var hash = ReadBytes(reader, 32);
                if (hash.Length != 32) throw new IOException("Invalid recovery audio digest.");
                var endByte = version >= 3 ? reader.ReadInt64() : 0;
                previousEnd = ValidateEndByte(endByte, captured, previousEnd);
                stored.Add(new(new(index, overlaps, text, endByte), length, hash));
                // Routine mutations and UI snapshots read only the manifest. Full pending-audio
                // validation belongs to Open/List; ReadAudio verifies the requested chunk itself.
                if (text is null && inspectAudio)
                {
                    var audioPath = AudioPath(id, index);
                    byte[] audio;
                    try { audio = ReadEncrypted(audioPath, MaxAudioBytes); }
                    catch (FileNotFoundException ex)
                    { throw new IOException("Recovery audio is missing or invalid.", ex); }
                    try
                    {
                        ValidateAudio(audio);
                        if (audio.Length != length ||
                            !CryptographicOperations.FixedTimeEquals(SHA256.HashData(audio), hash))
                            throw new IOException("Recovery audio does not match its manifest.");
                    }
                    finally { CryptographicOperations.ZeroMemory(audio); }
                }
            }
            // Legacy journals remain unbound; the caller must not infer a destination for them.
            var context = version >= 2 ? ReadString(reader, MaxContextBytes) : "";
            if (stream.Position != stream.Length) throw new IOException("Trailing recovery manifest data.");
            var state = new RecoveryState(id, created, updated, language, captured, tail, complete,
                stored.Select(c => c.Chunk).ToArray(), context);
            tail = null;
            return new(state, stored);
        }
        catch (Exception ex) when (ex is ArgumentException or OverflowException)
        {
            throw new IOException("Invalid recovery manifest.", ex);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(bytes);
            if (tail is not null) CryptographicOperations.ZeroMemory(tail);
        }
    }

    private void WriteManifest(Manifest manifest)
    {
        using var stream = new MemoryStream();
        try
        {
            using (var writer = new BinaryWriter(stream, Utf8, true))
            {
                var s = manifest.State;
                writer.Write(0x5650524A);
                writer.Write(3);
                WriteString(writer, s.Id);
                writer.Write(s.CreatedAt.UtcTicks);
                writer.Write(s.UpdatedAt.UtcTicks);
                WriteString(writer, s.Language);
                writer.Write(s.CapturedBytes);
                WriteBytes(writer, s.Tail);
                writer.Write(s.CaptureComplete);
                writer.Write(manifest.Stored.Count);
                foreach (var entry in manifest.Stored)
                {
                    writer.Write(entry.Chunk.Index);
                    writer.Write(entry.Chunk.OverlapsPrevious);
                    writer.Write(entry.Chunk.Text is not null);
                    if (entry.Chunk.Text is not null) WriteString(writer, entry.Chunk.Text);
                    writer.Write(entry.AudioBytes);
                    WriteBytes(writer, entry.Hash);
                    writer.Write(entry.Chunk.EndByte);
                }
                WriteString(writer, s.Context);
                writer.Flush();
            }
            var bytes = stream.ToArray();
            try { WriteEncrypted(Path.Combine(SessionPath(manifest.State.Id), ManifestName), bytes, MaxManifestBytes); }
            finally { CryptographicOperations.ZeroMemory(bytes); }
        }
        finally { CryptographicOperations.ZeroMemory(stream.GetBuffer()); }
    }

    private void WriteEncrypted(string path, byte[] plaintext, int limit)
    {
        if (plaintext.Length > limit) throw new IOException("Recovery file exceeds supported bounds.");
        byte[]? ciphertext = null;
        var copy = plaintext.ToArray();
        string? temporary = null;
        try
        {
            try { ciphertext = _protector.Protect(copy); }
            catch (Exception ex) when (ex is CryptographicException or ArgumentException)
            { throw new IOException("Recovery encryption failed.", ex); }
            if (ciphertext is null || ciphertext.Length == 0 || ciphertext.Length > limit + EncryptionAllowance)
                throw new IOException("Invalid encrypted recovery size.");
            GuardPath(path);
            EnsureQuota(ciphertext.Length);
            temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            GuardPath(temporary);
            using (var file = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                file.Write(ciphertext);
                file.Flush(true);
            }
            GuardPath(path);
            GuardPath(temporary);
            File.Move(temporary, path, true);
            temporary = null;
        }
        finally
        {
            CryptographicOperations.ZeroMemory(copy);
            if (ciphertext is not null) CryptographicOperations.ZeroMemory(ciphertext);
            if (temporary is not null)
            {
                GuardPath(temporary);
                File.Delete(temporary);
            }
        }
    }

    private byte[] ReadEncrypted(string path, int limit)
    {
        GuardPath(path);
        byte[]? ciphertext = null;
        byte[]? plaintext = null;
        try
        {
            using (var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read))
            {
                if (file.Length <= 0 || file.Length > limit + EncryptionAllowance || file.Length > _maxBytes)
                    throw new IOException("Encrypted recovery file exceeds supported bounds.");
                ciphertext = new byte[(int)file.Length];
                file.ReadExactly(ciphertext);
            }
            try { plaintext = _protector.Unprotect(ciphertext); }
            catch (Exception ex) when (ex is CryptographicException or ArgumentException)
            { throw new IOException("Recovery decryption failed.", ex); }
            if (plaintext is null || plaintext.Length > limit)
                throw new IOException("Recovery plaintext exceeds supported bounds.");
            // A test protector may return its input; the returned buffer must remain caller-owned.
            if (ReferenceEquals(plaintext, ciphertext)) plaintext = plaintext.ToArray();
            var result = plaintext;
            plaintext = null;
            return result;
        }
        finally
        {
            if (ciphertext is not null) CryptographicOperations.ZeroMemory(ciphertext);
            if (plaintext is not null) CryptographicOperations.ZeroMemory(plaintext);
        }
    }

    private void EnsureQuota(int additional)
    {
        long remaining = _maxBytes - additional;
        var directories = new Stack<string>();
        directories.Push(_root);
        var entries = 0;
        while (directories.TryPop(out var directory))
        {
            GuardPath(directory);
            // Enumeration supplies attributes and file lengths together on Windows. Avoid a
            // separate stat of every file and every ancestor for each checkpoint's quota check.
            foreach (var entry in new DirectoryInfo(directory).EnumerateFileSystemInfos())
            {
                if (++entries > 1000000) throw new IOException("Recovery storage has too many entries.");
                if ((entry.Attributes & FileAttributes.ReparsePoint) != 0)
                    throw new IOException("Reparse points are not supported in recovery storage.");
                if (entry is DirectoryInfo) directories.Push(entry.FullName);
                else remaining -= ((FileInfo)entry).Length;
                if (remaining < 0) throw new IOException("Recovery storage quota exceeded.");
            }
        }
        if (remaining < 0) throw new IOException("Recovery storage quota exceeded.");
    }

    private string SessionPath(string id)
    {
        if (!ValidId(id)) throw new ArgumentException("A recovery ID must be a lowercase GUID in N format.", nameof(id));
        return Path.Combine(_root, id);
    }

    private string AudioPath(string id, int index) => Path.Combine(SessionPath(id), $"{index:D8}.wav.enc");
    private static bool ValidId(string id) => id is not null && Guid.TryParseExact(id, "N", out var guid) &&
        StringComparer.Ordinal.Equals(id, guid.ToString("N"));

    private static void GuardPath(string path)
    {
        for (var current = Path.GetFullPath(path); current is not null; current = Path.GetDirectoryName(current))
        {
            try
            {
                if ((File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                    throw new IOException("Reparse points are not supported in recovery storage.");
            }
            catch (FileNotFoundException) { }
            catch (DirectoryNotFoundException) { }
        }
    }

    private static void ValidateLanguage(string language)
    {
        if (string.IsNullOrWhiteSpace(language) || Utf8.GetByteCount(language) > 128)
            throw new IOException("Invalid recovery language.");
    }

    private static long ValidateEndByte(long endByte, long capturedBytes, long previousEnd)
    {
        if (endByte < 0 || endByte > capturedBytes || (endByte != 0 && endByte < previousEnd))
            throw new IOException("Invalid recovery chunk source position.");
        return endByte == 0 ? previousEnd : endByte;
    }

    private static void ValidateAudio(byte[] audio)
    {
        if (audio is null || audio.Length < 46 || audio.Length > MaxAudioBytes ||
            (audio.Length - 44) % 2 != 0 || !audio.AsSpan(0, 4).SequenceEqual("RIFF"u8) ||
            !audio.AsSpan(8, 8).SequenceEqual("WAVEfmt "u8) ||
            BinaryPrimitives.ReadInt32LittleEndian(audio.AsSpan(4)) != audio.Length - 8 ||
            BinaryPrimitives.ReadInt32LittleEndian(audio.AsSpan(16)) != 16 ||
            BinaryPrimitives.ReadInt16LittleEndian(audio.AsSpan(20)) != 1 ||
            BinaryPrimitives.ReadInt16LittleEndian(audio.AsSpan(22)) != 1 ||
            BinaryPrimitives.ReadInt32LittleEndian(audio.AsSpan(24)) != PcmChunker.SampleRate ||
            BinaryPrimitives.ReadInt32LittleEndian(audio.AsSpan(28)) != PcmChunker.BytesPerSecond ||
            BinaryPrimitives.ReadInt16LittleEndian(audio.AsSpan(32)) != 2 ||
            BinaryPrimitives.ReadInt16LittleEndian(audio.AsSpan(34)) != 16 ||
            !audio.AsSpan(36, 4).SequenceEqual("data"u8) ||
            BinaryPrimitives.ReadInt32LittleEndian(audio.AsSpan(40)) != audio.Length - 44)
            throw new IOException("Invalid or oversized recovery PCM WAV.");
    }

    private static bool ReadBoolean(BinaryReader reader) => reader.ReadByte() switch
    {
        0 => false,
        1 => true,
        _ => throw new IOException("Invalid recovery flag.")
    };

    private static byte[] ReadBytes(BinaryReader reader, int max)
    {
        var count = reader.ReadInt32();
        if (count < 0 || count > max || count > reader.BaseStream.Length - reader.BaseStream.Position)
            throw new IOException("Invalid recovery field length.");
        return reader.ReadBytes(count);
    }

    private static string ReadString(BinaryReader reader, int max)
    {
        var bytes = ReadBytes(reader, max);
        try { return Utf8.GetString(bytes); }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    private static void WriteString(BinaryWriter writer, string text)
    {
        var bytes = Utf8.GetBytes(text);
        try { WriteBytes(writer, bytes); }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    private static void WriteBytes(BinaryWriter writer, byte[] bytes)
    {
        if (writer.BaseStream.Position + 4 + bytes.Length > MaxManifestBytes)
            throw new IOException("Recovery manifest exceeds supported bounds.");
        writer.Write(bytes.Length);
        writer.Write(bytes);
    }

    private static void Clear(Manifest manifest) => CryptographicOperations.ZeroMemory(manifest.State.Tail);
}

/// <summary>
/// Snapshot and audio buffers belong to the caller. Dispose only releases the cleanup lease;
/// it does not remove saved data. Same-text result retries are idempotent; conflicting results fail.
/// </summary>
public sealed class RecoveryJournal : IDisposable
{
    private readonly RecoveryStore _store;
    internal string Id { get; }
    internal bool IsDisposed { get; private set; }

    internal RecoveryJournal(RecoveryStore store, string id) { _store = store; Id = id; }
    public RecoveryState Snapshot => _store.Snapshot(this);
    public void Checkpoint(byte[] tail, long capturedBytes, IReadOnlyList<AudioChunk> added, bool complete = false) =>
        _store.Checkpoint(this, tail, capturedBytes, added, complete);
    public void SaveResult(int index, string text) => _store.SaveResult(this, index, text);
    public byte[] ReadAudio(int index) => _store.ReadAudio(this, index);
    public void Delete() => _store.Delete(Id);
    public void Dispose() => IsDisposed = true;
    internal void EnsureOpen() => ObjectDisposedException.ThrowIf(IsDisposed, this);
}
