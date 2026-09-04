namespace VoicePrompt.Core.Realtime;

/// <summary>
/// Bounded FIFO of already-handled <c>event_id</c> values. Web PubSub delivers at-least-once
/// and a reconnect can replay, so every event is checked before any side effect (clipboard,
/// toast, history) happens.
/// </summary>
public sealed class EventDeduplicator
{
    private readonly int _capacity;
    private readonly HashSet<string> _seen;
    private readonly Queue<string> _order;
    private readonly object _gate = new();

    public EventDeduplicator(int capacity = 512)
    {
        if (capacity <= 0)
        {
            throw new ArgumentOutOfRangeException(nameof(capacity));
        }

        _capacity = capacity;
        _seen = new HashSet<string>(StringComparer.Ordinal);
        _order = new Queue<string>(capacity);
    }

    public int Count
    {
        get { lock (_gate) { return _seen.Count; } }
    }

    /// <summary>
    /// Record <paramref name="eventId"/> and return <c>true</c> when it had not been seen
    /// before (i.e. the caller should process it). Empty ids are never deduped.
    /// </summary>
    public bool TryMarkSeen(string? eventId)
    {
        if (string.IsNullOrEmpty(eventId))
        {
            return true;
        }

        lock (_gate)
        {
            if (!_seen.Add(eventId))
            {
                return false;
            }

            _order.Enqueue(eventId);
            while (_order.Count > _capacity)
            {
                _seen.Remove(_order.Dequeue());
            }

            return true;
        }
    }

    public bool HasSeen(string? eventId)
    {
        if (string.IsNullOrEmpty(eventId))
        {
            return false;
        }

        lock (_gate) { return _seen.Contains(eventId); }
    }

    public void Clear()
    {
        lock (_gate)
        {
            _seen.Clear();
            _order.Clear();
        }
    }
}
