namespace VoicePrompt.Core.Realtime;

/// <summary>
/// Bounded exponential backoff with full jitter for realtime reconnects. Jitter avoids a
/// tight synchronized retry loop against the service; the cap keeps a long outage from
/// pushing the delay to an unusable value.
/// </summary>
public sealed class ReconnectPolicy
{
    public static readonly ReconnectPolicy Default = new();

    private readonly TimeSpan _initial;
    private readonly TimeSpan _max;
    private readonly double _factor;
    private readonly Func<double> _random;

    public ReconnectPolicy(
        TimeSpan? initial = null,
        TimeSpan? max = null,
        double factor = 2.0,
        Func<double>? random = null)
    {
        _initial = initial ?? TimeSpan.FromSeconds(1);
        _max = max ?? TimeSpan.FromSeconds(60);
        _factor = factor <= 1 ? 2.0 : factor;
        _random = random ?? Random.Shared.NextDouble;
    }

    public TimeSpan Initial => _initial;

    public TimeSpan Max => _max;

    /// <summary>Deterministic, un-jittered ceiling for attempt <paramref name="attempt"/> (1-based).</summary>
    public TimeSpan Ceiling(int attempt)
    {
        if (attempt < 1)
        {
            attempt = 1;
        }

        // Compute in double to avoid overflow, then clamp to the cap.
        var ms = _initial.TotalMilliseconds * Math.Pow(_factor, attempt - 1);
        if (double.IsInfinity(ms) || ms > _max.TotalMilliseconds)
        {
            return _max;
        }

        return TimeSpan.FromMilliseconds(ms);
    }

    /// <summary>
    /// Delay before attempt <paramref name="attempt"/> (1-based): a jittered value in
    /// <c>[50%, 100%]</c> of <see cref="Ceiling"/>, never above <see cref="Max"/>.
    /// </summary>
    public TimeSpan NextDelay(int attempt)
    {
        var ceiling = Ceiling(attempt);
        var jitter = 0.5 + (Math.Clamp(_random(), 0.0, 1.0) * 0.5);
        return TimeSpan.FromMilliseconds(ceiling.TotalMilliseconds * jitter);
    }
}
