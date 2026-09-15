using System.Text.Json;
using System.Text.Json.Serialization;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.History;

/// <summary>
/// One locally cached transcript. Text only; dictation audio is never persisted.
/// </summary>
public sealed class HistoryEntry
{
    [JsonPropertyName("transcript_id")] public string TranscriptId { get; init; } = "";
    [JsonPropertyName("recording_id")] public string RecordingId { get; init; } = "";
    [JsonPropertyName("preview")] public string Preview { get; init; } = "";
    [JsonPropertyName("body")] public string Body { get; init; } = "";
    [JsonPropertyName("completed_at")] public DateTimeOffset CompletedAt { get; init; }
    [JsonPropertyName("cached_at")] public DateTimeOffset CachedAt { get; init; }
    [JsonPropertyName("character_count")] public int CharacterCount { get; init; }
}

/// <summary>
/// Bounded, atomically-persisted local transcript cache mirroring the backend's 48-hour
/// retention. Entries older than <see cref="Retention"/> are dropped on load, on write, and
/// by the periodic cleanup tick; the newest <see cref="MaxEntries"/> are kept.
/// </summary>
public sealed class HistoryStore
{
    public static readonly TimeSpan Retention = TimeSpan.FromHours(48);
    public const int MaxEntries = 200;

    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = false,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private readonly string _path;
    private readonly IFileSystem _fs;
    private readonly IClock _clock;
    private readonly int _maxEntries;
    private readonly TimeSpan _retention;
    private readonly object _gate = new();

    private List<HistoryEntry>? _cache;

    public HistoryStore(
        string path,
        IFileSystem fs,
        IClock clock,
        int maxEntries = MaxEntries,
        TimeSpan? retention = null)
    {
        _path = path;
        _fs = fs;
        _clock = clock;
        _maxEntries = maxEntries;
        _retention = retention ?? Retention;
    }

    /// <summary>Raised whenever the persisted set changes (add / clear / cleanup removal).</summary>
    public event Action? Changed;

    /// <summary>All live entries, newest first.</summary>
    public IReadOnlyList<HistoryEntry> All()
    {
        lock (_gate)
        {
            return Live(LoadLocked(), _clock.UtcNow, _retention, _maxEntries);
        }
    }

    /// <summary>The most recently completed live entry, or <c>null</c>.</summary>
    public HistoryEntry? Latest() => All().FirstOrDefault();

    public HistoryEntry? Get(string transcriptId) =>
        All().FirstOrDefault(e => string.Equals(e.TranscriptId, transcriptId, StringComparison.Ordinal));

    /// <summary>Insert or replace an entry, then prune by retention and count.</summary>
    public void Add(HistoryEntry entry)
    {
        lock (_gate)
        {
            var items = LoadLocked()
                .Where(e => !string.Equals(e.TranscriptId, entry.TranscriptId, StringComparison.Ordinal))
                .ToList();
            items.Add(entry);
            SaveLocked(Live(items, _clock.UtcNow, _retention, _maxEntries));
        }

        Changed?.Invoke();
    }

    /// <summary>Drop expired entries. Returns the number removed.</summary>
    public int Cleanup()
    {
        int removed;
        lock (_gate)
        {
            var items = LoadLocked();
            var kept = Live(items, _clock.UtcNow, _retention, _maxEntries);
            removed = items.Count - kept.Count;
            if (removed > 0)
            {
                SaveLocked(kept);
            }
        }

        if (removed > 0)
        {
            Changed?.Invoke();
        }

        return removed;
    }

    /// <summary>Delete all cached transcripts.</summary>
    public void Clear()
    {
        lock (_gate)
        {
            _cache = new List<HistoryEntry>();
            _fs.Delete(_path);
        }

        Changed?.Invoke();
    }

    private static List<HistoryEntry> Live(
        IEnumerable<HistoryEntry> items, DateTimeOffset now, TimeSpan retention, int max) =>
        items
            .Where(e => !string.IsNullOrEmpty(e.TranscriptId))
            .Where(e => now - e.CompletedAt < retention)
            .OrderByDescending(e => e.CompletedAt)
            .Take(max)
            .ToList();

    private List<HistoryEntry> LoadLocked()
    {
        if (_cache is not null)
        {
            return _cache;
        }

        if (!_fs.FileExists(_path))
        {
            return _cache = new List<HistoryEntry>();
        }

        try
        {
            _cache = JsonSerializer.Deserialize<List<HistoryEntry>>(_fs.ReadAllText(_path), Options)
                     ?? new List<HistoryEntry>();
        }
        catch
        {
            // Corrupt cache is not fatal — the backend remains the source of truth.
            _cache = new List<HistoryEntry>();
        }

        return _cache;
    }

    private void SaveLocked(List<HistoryEntry> items)
    {
        _cache = items;
        _fs.AtomicWrite(_path, JsonSerializer.Serialize(items, Options));
    }
}
