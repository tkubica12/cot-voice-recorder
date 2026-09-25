using System.Text;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Dictation;

public sealed record DictationEdit(string Original, string Replacement);
public sealed record DictationRefinement(string Text, IReadOnlyList<DictationEdit> Edits);

/// <summary>
/// PendingBlocks is zero or one unresolved response, not an unsent tail or a timed-out physical call.
/// SuccessfulBlocks counts validated responses (including unchanged text); UnprocessedWords counts
/// original whitespace-delimited words not fully covered by any successful response.
/// LastFailure is a sanitized warning, never service exception or transcript text.
/// </summary>
public sealed record DictationPolishProgress(string Text, int PendingBlocks, int FallbackBlocks,
    int SuccessfulBlocks = 0, int UnprocessedWords = 0, string? LastFailure = null);
public sealed record DictationPolishResult(string Text, string RawText, int FallbackBlocks,
    int SuccessfulBlocks = 0, int UnprocessedWords = 0, string? LastFailure = null);

/// <summary>
/// Rolling refinement. All ranges, edits and context are anchored to original raw UTF-16 offsets.
/// Only one physical call exists, and no snapshots are queued. Update also acts as the clock tick
/// for low-volume speech. CompleteAsync freezes immediately; CompleteWithinAsync can use a bounded
/// stop-time opportunity for the active request and one final request for unsubmitted speech.
/// Edit sets supersede intersecting prior edits, preserving disjoint corrections. Empty validated
/// edit sets preserve all prior corrections; an explicit identity edit can revert a correction.
/// </summary>
public sealed class DictationPolisher : IAsyncDisposable
{
    public static readonly TimeSpan DefaultCallTimeout = TimeSpan.FromSeconds(25);
    private const int MaxWindowCharacters = 4000;
    private readonly object _gate = new();
    private readonly Func<string, string, CancellationToken, Task<DictationRefinement>> _refine;
    private readonly int _targetWords;
    private readonly int _overlapWords;
    private readonly TimeSpan _pendingDelay;
    private readonly bool _adaptiveStart;
    private readonly TimeSpan _callTimeout;
    private readonly TimeSpan _maxWindowAge;
    private readonly TimeProvider _clock;
    private readonly ILog _log;
    private readonly List<Patch> _patches = [];
    private readonly List<Coverage> _coverage = [];
    private readonly Queue<Arrival> _arrivals = new();
    private string _raw = "";
    private int _frontier;
    private int _lastSubmittedEnd;
    private int _tooOldEnd;
    private long? _pendingSince;
    private long? _stopTimestamp;
    private TimeSpan? _stopBudget;
    private Call? _active;
    private int _fallbacks;
    private int _successes;
    private string? _lastFailure;
    private bool _stopped;
    private bool _invalidated;
    private bool _frozen;
    private bool _disposed;
    private DictationPolishProgress _progress = new("", 0, 0);
    private Task<DictationPolishResult>? _completion;

    public DictationPolisher(
        Func<string, string, CancellationToken, Task<DictationRefinement>> refine,
        int targetWords = 75, int overlapWords = 15, TimeSpan? pendingDelay = null,
        TimeSpan? callTimeout = null, TimeProvider? timeProvider = null, TimeSpan? maxWindowAge = null,
        ILog? log = null)
    {
        ArgumentNullException.ThrowIfNull(refine);
        if (targetWords < 2 || targetWords > 150) throw new ArgumentOutOfRangeException(nameof(targetWords));
        if (overlapWords < 0 || overlapWords > 20) throw new ArgumentOutOfRangeException(nameof(overlapWords));
        _pendingDelay = pendingDelay ?? TimeSpan.FromSeconds(12);
        _adaptiveStart = targetWords == 75 && pendingDelay is null;
        _callTimeout = callTimeout ?? DefaultCallTimeout;
        _maxWindowAge = maxWindowAge ?? TimeSpan.FromSeconds(30);
        ValidateDuration(_pendingDelay, nameof(pendingDelay));
        ValidateDuration(_callTimeout, nameof(callTimeout));
        ValidateDuration(_maxWindowAge, nameof(maxWindowAge));
        if (_maxWindowAge <= _pendingDelay) throw new ArgumentOutOfRangeException(nameof(maxWindowAge));
        _targetWords = targetWords;
        _overlapWords = Math.Min(overlapWords, targetWords - 1);
        _refine = refine;
        _clock = timeProvider ?? TimeProvider.System;
        _log = log ?? NullLog.Instance;
    }

    public DictationPolishProgress Progress => Volatile.Read(ref _progress);

    /// <summary>Appends raw text or ticks the pending-age trigger. The delegate runs off-thread.</summary>
    public void Update(string rawText)
    {
        ArgumentNullException.ThrowIfNull(rawText);
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (_frozen) return;
            UpdateRaw(rawText);
            Dispatch();
            Publish();
        }
    }

    /// <summary>Stops all future dispatch, including dispatch after the active call completes.</summary>
    public void StopScheduling(TimeSpan? stopBudget = null)
    {
        if (stopBudget < TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(stopBudget));
        lock (_gate)
        {
            _stopTimestamp ??= _clock.GetTimestamp();
            _stopBudget ??= stopBudget;
            _stopped = true;
            if (_active is { Started: false } call)
            {
                call.Skipped = true;
                call.Cancel();
            }
            Publish();
        }
    }

    /// <summary>
    /// Freezes synchronously and returns an already-completed task. Cancellation freezes first and
    /// returns a cancelled task. Repeated calls return the same task, even with different arguments.
    /// An unfinished call or unsent tail does not increment FallbackBlocks.
    /// </summary>
    public Task<DictationPolishResult> CompleteAsync(string rawText, CancellationToken ct)
    {
        ArgumentNullException.ThrowIfNull(rawText);
        lock (_gate)
        {
            if (_completion is not null) return _completion;
            ObjectDisposedException.ThrowIf(_disposed, this);
            UpdateRaw(rawText);
            Freeze();
            var result = new DictationPolishResult(_progress.Text, _raw, _fallbacks,
                _successes, _progress.UnprocessedWords, _lastFailure);
            return _completion = ct.IsCancellationRequested
                ? Task.FromCanceled<DictationPolishResult>(ct)
                : Task.FromResult(result);
        }
    }

    public Task<DictationPolishResult> CompleteWithinAsync(string rawText, TimeSpan budget, CancellationToken ct)
    {
        ArgumentNullException.ThrowIfNull(rawText);
        if (budget < TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(budget));
        lock (_gate)
        {
            if (_completion is not null) return _completion;
            ObjectDisposedException.ThrowIf(_disposed, this);
            UpdateRaw(rawText);
            _stopTimestamp ??= _clock.GetTimestamp();
            _stopBudget ??= budget;
            _stopped = true;
            return _completion = FinishWithinCoreAsync(budget, ct);
        }
    }

    private async Task<DictationPolishResult> FinishWithinCoreAsync(TimeSpan budget, CancellationToken ct)
    {
        var started = _clock.GetTimestamp();
        try
        {
            while (budget > _clock.GetElapsedTime(started))
            {
                ct.ThrowIfCancellationRequested();
                Task? pending;
                lock (_gate)
                {
                    if (_frozen) break;
                    if (_active is null) Dispatch(force: true);
                    pending = _active?.Released.Task;
                }
                if (pending is null) break;
                var remaining = budget - _clock.GetElapsedTime(started);
                if (remaining <= TimeSpan.Zero) break;
                try { await pending.WaitAsync(remaining, _clock, ct).ConfigureAwait(false); }
                catch (TimeoutException) { break; }
            }
        }
        finally
        {
            lock (_gate) Freeze();
        }
        ct.ThrowIfCancellationRequested();
        lock (_gate)
            return new DictationPolishResult(_progress.Text, _raw, _fallbacks,
                _successes, _progress.UnprocessedWords, _lastFailure);
    }

    private static void ValidateDuration(TimeSpan duration, string name)
    {
        if (duration <= TimeSpan.Zero || duration.TotalMilliseconds > uint.MaxValue - 1)
            throw new ArgumentOutOfRangeException(name);
    }

    private void UpdateRaw(string rawText)
    {
        if (!rawText.StartsWith(_raw, StringComparison.Ordinal))
        {
            _patches.Clear();
            _coverage.Clear();
            _arrivals.Clear();
            if (!_invalidated) Fail("Raw transcript prefix changed; refinement disabled for this session.");
            _invalidated = true;
            _active?.Cancel();
        }
        if (rawText.Length > _raw.Length && rawText.Length > _lastSubmittedEnd
            && !string.IsNullOrWhiteSpace(rawText[_raw.Length..]))
            _pendingSince ??= _clock.GetTimestamp();
        if (!_invalidated && rawText.Length > _raw.Length)
            _arrivals.Enqueue(new(rawText.Length, _clock.GetTimestamp()));
        _raw = rawText;
        ExpireArrivals();
    }

    private void ExpireArrivals()
    {
        while (_arrivals.TryPeek(out var arrival)
            && _clock.GetElapsedTime(arrival.Timestamp) >= _maxWindowAge)
        {
            _tooOldEnd = arrival.End;
            _arrivals.Dequeue();
        }
    }

    private void Dispatch(bool force = false)
    {
        if ((_stopped && !force) || _frozen || _invalidated || _active is not null
            || _raw.Length <= _lastSubmittedEnd) return;
        if (string.IsNullOrWhiteSpace(_raw[_lastSubmittedEnd..])) return;
        ExpireArrivals();
        // A growing oversized token previously passed through raw is never sent as a suffix.
        if (_frontier > 0 && _frontier < _raw.Length
            && !char.IsWhiteSpace(_raw[_frontier - 1]) && !char.IsWhiteSpace(_raw[_frontier]))
        {
            var old = _frontier;
            while (_frontier < _raw.Length && !char.IsWhiteSpace(_raw[_frontier])) _frontier++;
            if (!IsCovered(new(old, _frontier)))
                Fail("Oversized or partial token retained raw; token fragments cannot be refined.");
            _lastSubmittedEnd = Math.Max(_lastSubmittedEnd, _frontier);
        }
        var words = Tokens(_raw, _frontier, _raw.Length);
        if (words.Count == 0) return;
        var triggerWords = _adaptiveStart ? _successes switch { 0 => 20, 1 => 40, _ => 75 } : _targetWords;
        var delay = _adaptiveStart ? _successes switch
        {
            0 => TimeSpan.FromSeconds(4),
            1 => TimeSpan.FromSeconds(6),
            _ => _pendingDelay,
        } : _pendingDelay;
        var ageReady = _pendingSince is long since && _clock.GetElapsedTime(since) >= delay;
        var readyWords = _adaptiveStart ? Tokens(_raw, _lastSubmittedEnd, _raw.Length).Count : words.Count;
        if (!force && readyWords < triggerWords && !ageReady && _raw.Length - _frontier < MaxWindowCharacters)
            return;

        // Select the latest bounded window, never a FIFO backlog. Old unsubmitted speech remains
        // visible verbatim and is explicitly reported as a fallback.
        var first = Math.Max(0, words.Count - _targetWords);
        var start = first == 0 ? _frontier : words[first].Start;
        var end = words[^1].End;
        if (end - start > MaxWindowCharacters && end - words[first].Start <= MaxWindowCharacters)
            start = words[first].Start;
        while (first < words.Count && end - start > MaxWindowCharacters)
        {
            first++;
            start = first < words.Count ? words[first].Start : end;
        }
        var agedOut = _tooOldEnd > start;
        start = Math.Max(start, Math.Min(end, _tooOldEnd));
        // A prior accepted edit is indivisible. Commit it whole if it cannot fit in this window.
        int previousStart;
        do
        {
            previousStart = start;
            if (start > 0 && start < end && !char.IsWhiteSpace(_raw[start - 1]))
                while (start < end && !char.IsWhiteSpace(_raw[start])) start++;
            foreach (var patch in _patches)
                if (patch.Start < start && start < patch.End) start = patch.End;
        } while (start != previousStart);
        if (start > _frontier)
        {
            if (Tokens(_raw, _frontier, start).Any(word => !IsCovered(word)))
                Fail(agedOut
                    ? "Unsubmitted speech became too old; older speech retained raw."
                    : "Unsubmitted speech exceeded the rolling window; older speech retained raw.");
            _frontier = start;
        }
        if (start == end)
        {
            _lastSubmittedEnd = end;
            _pendingSince = null;
            return;
        }
        // Include only bounded trailing whitespace; it remains verbatim outside the request otherwise.
        while (end < _raw.Length && char.IsWhiteSpace(_raw[end]) && end - start < MaxWindowCharacters)
            end++;
        var overlapWords = _adaptiveStart ? Math.Min(_overlapWords,
            _successes switch { 0 => 5, 1 => 10, _ => _overlapWords }) : _overlapWords;
        var call = new Call(start, end, _raw[start..end], ContextBefore(start), overlapWords, force);
        _active = call;
        _lastSubmittedEnd = end;
        _pendingSince = end < _raw.Length ? _clock.GetTimestamp() : null;
        _ = Task.Run(() => ExecuteAsync(call));
    }

    private async Task ExecuteAsync(Call call)
    {
        // This extra task also isolates a delegate which blocks before returning its Task.
        var underlying = Task.Run(async () =>
        {
            lock (_gate)
            {
                if (_frozen || _invalidated || (_stopped && !call.Final) || call.Resolved)
                {
                    call.Skipped = true;
                    throw new OperationCanceledException();
                }
                call.Started = true;
            }
            call.Token.ThrowIfCancellationRequested();
            return await _refine(call.Raw, call.Context, call.Token).ConfigureAwait(false);
        });
        List<Patch>? edits = null;
        string? failure = null;
        try
        {
            var response = await underlying.WaitAsync(_callTimeout, _clock, call.Token).ConfigureAwait(false);
            edits = Validate(call, response);
            if (edits is null) failure = "Invalid refinement edit provenance; submitted speech retained raw.";
        }
        catch (TimeoutException) { failure = "Refinement request timed out; submitted speech retained raw."; }
        catch (OperationCanceledException) { failure = "Refinement request was cancelled; submitted speech retained raw."; }
        catch (Exception) { failure = "Refinement service failed; submitted speech retained raw."; }

        lock (_gate)
        {
            call.Resolved = true;
            var onTime = _stopTimestamp is not long stopped || _stopBudget is not TimeSpan limit
                || _clock.GetElapsedTime(stopped) <= limit;
            if (!_frozen && !_invalidated && !call.Skipped && onTime)
            {
                if (edits is not null)
                {
                    // Merge only original-indexed provenance, never corrected text or fuzzy matches.
                    _patches.RemoveAll(p => p.Start >= call.Start && p.End <= call.End
                        && edits.Any(edit => edit.Start < p.End && p.Start < edit.End));
                    _patches.AddRange(edits);
                    _patches.Sort((a, b) => a.Start.CompareTo(b.Start));
                    AddCoverage(call.Start, call.End);
                    _successes++;
                }
                else Fail(failure!);
                AdvanceFrontier(call);
                Publish();
            }
        }
        if (!underlying.IsCompleted) call.Cancel();
        // Timeout/cancellation does not release the physical slot of a cancellation-ignoring call.
        try { await underlying.ConfigureAwait(false); }
        catch (Exception) { }
        lock (_gate)
        {
            _active = null;
            Dispatch();
            Publish();
            call.Released.TrySetResult();
        }
        await call.DisposeAsync().ConfigureAwait(false);
    }

    private void AdvanceFrontier(Call call)
    {
        var words = Tokens(_raw, call.Start, call.End);
        var keep = Math.Min(call.OverlapWords, words.Count);
        var frontier = keep == 0 ? call.End : words[words.Count - keep].Start;
        int previousFrontier;
        do
        {
            previousFrontier = frontier;
            foreach (var patch in _patches)
                if (patch.Start < frontier && frontier < patch.End) frontier = patch.Start;
            if (keep > 0)
                while (frontier > call.Start && !char.IsWhiteSpace(_raw[frontier - 1])) frontier--;
        } while (frontier != previousFrontier);
        _frontier = frontier;
        while (_arrivals.TryPeek(out var arrival) && arrival.End <= _frontier)
            _arrivals.Dequeue();
    }

    private static List<Patch>? Validate(Call call, DictationRefinement? response)
    {
        if (response?.Text is null || response.Edits is null || response.Text.Length > 8000
            || response.Edits.Count > MaxWindowCharacters || !ValidText(response.Text)) return null;
        var patches = new List<Patch>();
        foreach (var edit in response.Edits)
        {
            if (edit is null || string.IsNullOrEmpty(edit.Original) || edit.Replacement is null
                || edit.Original.Length > call.Raw.Length || edit.Replacement.Length > 8000
                || !ValidText(edit.Original) || !ValidText(edit.Replacement)) return null;
            var start = call.Raw.IndexOf(edit.Original, StringComparison.Ordinal);
            if (start < 0 || call.Raw.IndexOf(edit.Original, start + 1, StringComparison.Ordinal) >= 0)
                return null;
            var end = start + edit.Original.Length;
            if (!Utf16Boundary(call.Raw, start) || !Utf16Boundary(call.Raw, end)) return null;
            // Deleting a filler including both separators must not concatenate surrounding words.
            if (edit.Replacement.Length == 0 && start > 0 && end < call.Raw.Length
                && edit.Original.Any(char.IsWhiteSpace)
                && char.IsLetterOrDigit(call.Raw[start - 1]) && char.IsLetterOrDigit(call.Raw[end]))
                return null;
            patches.Add(new(call.Start + start, call.Start + end, edit.Replacement));
        }
        patches.Sort((a, b) => a.Start.CompareTo(b.Start));
        var assembled = new StringBuilder();
        var position = call.Start;
        foreach (var patch in patches)
        {
            if (patch.Start < position) return null;
            if (assembled.Length + patch.Start - position + patch.Replacement.Length > 8000) return null;
            assembled.Append(call.Raw.AsSpan(position - call.Start, patch.Start - position));
            assembled.Append(patch.Replacement);
            position = patch.End;
        }
        assembled.Append(call.Raw.AsSpan(position - call.Start));
        return assembled.ToString() == response.Text ? patches : null;
    }

    private static bool Utf16Boundary(string text, int offset) =>
        offset == 0 || offset == text.Length
        || !(char.IsHighSurrogate(text[offset - 1]) && char.IsLowSurrogate(text[offset]));

    private static bool ValidText(string text)
    {
        for (var i = 0; i < text.Length; i++)
        {
            if (char.IsControl(text[i]) && !char.IsWhiteSpace(text[i])) return false;
            if (char.IsHighSurrogate(text[i]))
            {
                if (++i == text.Length || !char.IsLowSurrogate(text[i])) return false;
            }
            else if (char.IsLowSurrogate(text[i])) return false;
        }
        return true;
    }

    private string ContextBefore(int end)
    {
        var lower = Math.Max(0, end - 8000);
        if (!Utf16Boundary(_raw, lower)) lower++;
        if (lower > 0)
            while (lower < end && !char.IsWhiteSpace(_raw[lower - 1])) lower++;
        var words = Tokens(_raw, lower, end);
        var start = words.Count > 150 ? words[^150].Start : lower;
        return _raw[start..end];
    }

    private static List<Coverage> Tokens(string text, int start, int end)
    {
        var result = new List<Coverage>();
        while (start < end)
        {
            while (start < end && char.IsWhiteSpace(text[start])) start++;
            if (start == end) break;
            var first = start;
            while (start < end && !char.IsWhiteSpace(text[start])) start++;
            result.Add(new(first, start));
        }
        return result;
    }

    private void AddCoverage(int start, int end)
    {
        if (_coverage.Count > 0 && _coverage[^1].End >= start)
            _coverage[^1] = new(_coverage[^1].Start, Math.Max(end, _coverage[^1].End));
        else _coverage.Add(new(start, end));
    }

    private bool IsCovered(Coverage word) =>
        _coverage.Any(range => range.Start <= word.Start && range.End >= word.End);

    private void Fail(string reason)
    {
        _fallbacks++;
        _lastFailure = reason;
        _log.Warn($"dictation AI: fallback-block={_fallbacks}; reason={reason}");
    }

    private void Publish()
    {
        if (_frozen) return;
        var text = new StringBuilder(_raw.Length);
        var position = 0;
        foreach (var patch in _patches)
        {
            text.Append(_raw.AsSpan(position, patch.Start - position));
            text.Append(patch.Replacement);
            position = patch.End;
        }
        text.Append(_raw.AsSpan(position));
        var unprocessed = 0;
        var coverage = 0;
        foreach (var word in Tokens(_raw, 0, _raw.Length))
        {
            while (coverage < _coverage.Count && _coverage[coverage].End < word.End) coverage++;
            if (coverage == _coverage.Count || _coverage[coverage].Start > word.Start) unprocessed++;
        }
        Volatile.Write(ref _progress, new(text.ToString(),
            !_invalidated && !_stopped && _active is { Resolved: false } ? 1
                : !_invalidated && _active is { Started: true, Resolved: false } ? 1 : 0,
            _fallbacks, _successes, unprocessed, _lastFailure));
    }

    private void Freeze()
    {
        if (_frozen) return;
        _stopped = true;
        Publish();
        Volatile.Write(ref _progress, _progress with { PendingBlocks = 0 });
        _frozen = true;
        _active?.Cancel();
    }

    public ValueTask DisposeAsync()
    {
        lock (_gate)
        {
            if (_disposed) return ValueTask.CompletedTask;
            Freeze();
            _disposed = true;
        }
        return ValueTask.CompletedTask;
    }

    private sealed record Patch(int Start, int End, string Replacement);
    private sealed record Coverage(int Start, int End);
    private sealed record Arrival(int End, long Timestamp);

    private sealed class Call(int start, int end, string raw, string context, int overlapWords, bool final)
    {
        private readonly object _gate = new();
        private readonly CancellationTokenSource _cancel = new();
        private Task? _cancelling;
        public int Start { get; } = start;
        public int End { get; } = end;
        public string Raw { get; } = raw;
        public string Context { get; } = context;
        public int OverlapWords { get; } = overlapWords;
        public bool Final { get; } = final;
        public TaskCompletionSource Released { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public bool Started { get; set; }
        public bool Skipped { get; set; }
        public bool Resolved { get; set; }
        public CancellationToken Token => _cancel.Token;

        public void Cancel()
        {
            lock (_gate)
                _cancelling ??= Task.Run(async () =>
                {
                    try { await _cancel.CancelAsync().ConfigureAwait(false); }
                    catch (Exception) { }
                });
        }

        public async Task DisposeAsync()
        {
            Task? cancelling;
            lock (_gate) cancelling = _cancelling;
            if (cancelling is not null) await cancelling.ConfigureAwait(false);
            _cancel.Dispose();
        }
    }
}
