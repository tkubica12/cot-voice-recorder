using System.Collections.Concurrent;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public sealed class DictationPolisherTests
{
    [Fact]
    public async Task Pending_raw_triggers_at_twelve_seconds_on_update_tick_not_at_stop()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, timeProvider: clock);
        const string raw = "a short utterance";
        polisher.Update(raw);
        clock.Advance(TimeSpan.FromSeconds(11));
        polisher.Update(raw);
        Assert.Empty(requests.Items);
        Assert.Equal(0, polisher.Progress.PendingBlocks);
        clock.Advance(TimeSpan.FromSeconds(1));
        polisher.Update(raw);
        var call = await requests.At(0);
        Assert.Equal(raw, call.Raw);
        call.Finish(Replace(raw, "short", "brief"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        Assert.Equal("a brief utterance", polisher.Progress.Text);
        clock.Advance(TimeSpan.FromMinutes(1));
        polisher.Update(raw);
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Single(requests.Items);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(0, result.UnprocessedWords);
    }

    [Fact]
    public async Task Four_word_timed_window_stays_wholly_mutable_and_unrelated_deletion_preserves_corrections()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, timeProvider: clock);
        const string raw = "please open teh documment ";
        polisher.Update(raw);
        clock.Advance(TimeSpan.FromSeconds(15));
        polisher.Update(raw);
        var first = await requests.At(0);
        Assert.Equal(raw, first.Raw);
        first.Finish(new("please open the document ",
            [new("teh", "the"), new("documment", "document")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        for (var i = 0; i < 10; i++) polisher.Update(raw);
        Assert.Single(requests.Items);

        const string appended = raw + "and uh save ";
        polisher.Update(appended);
        clock.Advance(TimeSpan.FromSeconds(12));
        polisher.Update(appended);
        var second = await requests.At(1);
        Assert.Equal(appended, second.Raw);
        Assert.Equal("", second.Context);
        Assert.Equal("please open the document and uh save ", polisher.Progress.Text);
        second.Finish(Replace(second.Raw, "uh ", ""));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("please open the document and save ", polisher.Progress.Text);

        clock.Advance(TimeSpan.FromSeconds(60));
        for (var i = 0; i < 10; i++) polisher.Update(appended);
        var result = await polisher.CompleteAsync(appended, CancellationToken.None);
        Assert.Equal(2, requests.Items.Count);
        Assert.Equal("please open the document and save ", result.Text);
        Assert.Equal(appended, result.RawText);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(0, result.UnprocessedWords);
    }

    [Fact]
    public async Task Nonempty_new_edits_can_explicitly_revert_one_fix_without_dropping_disjoint_fixes()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 3);
        const string raw = "one two three teh documment uh ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(new("one two three the document uh ",
            [new("teh", "the"), new("documment", "document")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "four five six ");
        var second = await requests.At(1);
        Assert.Equal("teh documment uh four five six ", second.Raw);
        second.Finish(new("teh documment four five six ", [new("teh", "teh"), new("uh ", "")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("one two three teh document four five six ", polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task Sufficient_words_trigger_without_waiting_for_time()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine);
        var raw = Words(0, 75);
        polisher.Update(raw);
        var call = await requests.At(0);
        Assert.Equal(raw, call.Raw);
        call.Finish(Unchanged(call.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task Whitespace_arrivals_do_not_reprocess_overlap_or_bisect_a_trailing_edit()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, timeProvider: clock);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var call = await requests.At(0);
        call.Finish(Replace(raw, "four ", "FOUR "));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        clock.Advance(TimeSpan.FromSeconds(15));
        polisher.Update(raw + "\r\n\t ");
        Assert.Single(requests.Items);
        Assert.Equal("one two three FOUR \r\n\t ", polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task One_physical_request_and_no_stale_snapshot_queue()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, overlapWords: 1);
        var raw = Words(0, 4);
        polisher.Update(raw);
        var first = await requests.At(0);
        for (var i = 4; i < 24; i++)
        {
            raw += Words(i, 1);
            polisher.Update(raw);
            Assert.Equal(1, polisher.Progress.PendingBlocks);
        }
        Assert.Single(requests.Items);
        first.Finish(Replace(first.Raw, "w0", "W0"));
        var second = await requests.At(1);
        Assert.Equal(Words(20, 4), second.Raw);
        Assert.Equal(1, polisher.Progress.FallbackBlocks);
        Assert.StartsWith("W0 ", polisher.Progress.Text);
        Assert.Contains(Words(4, 16), polisher.Progress.Text);
        Assert.Equal(1, requests.Peak);
        second.Finish(Unchanged(second.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        polisher.StopScheduling();
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Equal(raw.Replace("w0", "W0", StringComparison.Ordinal), result.Text);
        Assert.Equal(16, result.UnprocessedWords);
        Assert.Equal(2, requests.Items.Count);
        Assert.Equal(1, requests.Peak);
    }

    [Fact]
    public async Task Overlap_correction_survives_updates_and_new_snapshot_replaces_only_submitted_range()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 2);
        const string firstRaw = "one two three four wrong six ";
        polisher.Update(firstRaw);
        var first = await requests.At(0);
        first.Finish(Replace(first.Raw, "wrong", "correct"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var appended = firstRaw + "seven eight nine ten ";
        polisher.Update(appended);
        var second = await requests.At(1);
        Assert.Equal("wrong six seven eight nine ten ", second.Raw);
        Assert.Equal("one two three four ", second.Context);
        Assert.Equal(appended.Replace("wrong", "correct", StringComparison.Ordinal), polisher.Progress.Text);
        var concurrent = appended + "eleven twelve ";
        polisher.Update(concurrent);
        Assert.Contains("correct", polisher.Progress.Text);
        second.Finish(Replace(second.Raw, "wrong six seven", "repaired six SEVEN"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("one two three four repaired six SEVEN eight nine ten eleven twelve ",
            polisher.Progress.Text);
        var result = await polisher.CompleteAsync(concurrent, CancellationToken.None);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(2, result.UnprocessedWords);
        Assert.Equal(concurrent, result.RawText);
    }

    [Fact]
    public async Task Deletion_changes_output_lengths_but_not_original_offsets_or_append()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 2);
        const string raw = "one uh two three four five ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(first.Raw, "uh ", ""));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var appended = raw + "six seven eight nine ";
        polisher.Update(appended);
        var second = await requests.At(1);
        Assert.Equal("four five six seven eight nine ", second.Raw);
        Assert.Equal("one uh two three ", second.Context);
        second.Finish(Replace(second.Raw, "five six", "FIVE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        var result = await polisher.CompleteAsync(appended + "tail", CancellationToken.None);
        Assert.Equal("one two three four FIVE seven eight nine tail", result.Text);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(1, result.UnprocessedWords);
    }

    [Fact]
    public async Task Commit_frontier_never_bisects_an_accepted_edit()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 2);
        const string raw = "one two three four five six ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(first.Raw, "four five", "FOUR-FIVE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "seven eight nine ");
        var second = await requests.At(1);
        Assert.Equal("four five six seven eight nine ", second.Raw);
        Assert.Equal("one two three ", second.Context);
        Assert.Contains("FOUR-FIVE", polisher.Progress.Text);
        second.Finish(Replace(second.Raw, "four five", "FOUR AND FIVE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("one two three FOUR AND FIVE six seven eight nine ", polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task Large_append_commits_crossing_patch_whole_instead_of_splitting_it()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 2);
        const string raw = "one two three four five six ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(first.Raw, "four five", "FOUR-FIVE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "seven eight nine ten ");
        var second = await requests.At(1);
        Assert.Equal(" six seven eight nine ten ", second.Raw);
        second.Finish(Unchanged(second.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("one two three FOUR-FIVE six seven eight nine ten ", polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task Frontier_adjustment_preserves_adjacent_edits_within_shared_words()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 3);
        const string raw = "one ab cd ef gh ij ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(new("one AB CDE Ff gh ij ", [new("ab c", "AB C"), new("d e", "DE F")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "kl ");
        var second = await requests.At(1);
        Assert.Equal("ab cd ef gh ij kl ", second.Raw);
        Assert.Equal("one ", second.Context);
        second.Finish(new("AB CDE Ff gh ij kl ", [new("ab c", "AB C"), new("d e", "DE F")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("one AB CDE Ff gh ij kl ", polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
    }

    [Fact]
    public async Task Failed_overlap_response_does_not_erase_an_earlier_valid_fix()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, overlapWords: 2);
        const string raw = "one two wrong four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(first.Raw, "wrong", "right"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "five six ");
        var second = await requests.At(1);
        second.Finish(new("fabricated", []));
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        Assert.Equal("one two right four five six ", polisher.Progress.Text);
        Assert.Contains("provenance", polisher.Progress.LastFailure);
    }

    [Fact]
    public async Task Empty_edit_set_preserves_corrections_both_inside_and_before_overlap()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, overlapWords: 1);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(new("ONE two three FOUR ", [new("one", "ONE"), new("four", "FOUR")]));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "five six seven ");
        var second = await requests.At(1);
        Assert.Equal("four five six seven ", second.Raw);
        second.Finish(Unchanged(second.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        Assert.Equal("ONE two three FOUR five six seven ", polisher.Progress.Text);
        var result = await polisher.CompleteAsync(raw + "five six seven ", CancellationToken.None);
        Assert.Equal("ONE two three FOUR five six seven ", result.Text);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(0, result.UnprocessedWords);
        Assert.Equal(2, result.SuccessfulBlocks);
    }

    [Fact]
    public async Task Empty_edit_set_preserves_length_changing_deletion_in_overlap_and_concurrent_append()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 6, overlapWords: 3);
        const string raw = "one two three um four five ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(raw, "um ", ""));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        const string appended = raw + "six seven eight ";
        polisher.Update(appended);
        var second = await requests.At(1);
        Assert.Equal("um four five six seven eight ", second.Raw);
        polisher.Update(appended + "tail");
        second.Finish(Unchanged(second.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 2);
        var result = await polisher.CompleteAsync(appended + "tail", CancellationToken.None);
        Assert.Equal("one two three four five six seven eight tail", result.Text);
        Assert.Equal(appended + "tail", result.RawText);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(1, result.UnprocessedWords);
    }

    [Fact]
    public async Task Stop_before_a_request_and_later_updates_never_dispatch_or_warn()
    {
        var requests = new Requests();
        var clock = new ManualClock();
        await using var polisher = new DictationPolisher(requests.Refine, timeProvider: clock);
        polisher.Update("short ");
        polisher.StopScheduling();
        clock.Advance(TimeSpan.FromMinutes(1));
        var raw = "short " + Words(0, 150);
        polisher.Update(raw);
        var task = polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.True(task.IsCompletedSuccessfully);
        var result = await task;
        Assert.Empty(requests.Items);
        Assert.Equal(raw, result.Text);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(151, result.UnprocessedWords);
        Assert.Null(result.LastFailure);
    }

    [Fact]
    public async Task Stop_during_request_accepts_result_but_never_dispatches_arrivals()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        polisher.StopScheduling();
        var appended = raw + Words(0, 100);
        polisher.Update(appended);
        Assert.False(first.Token.IsCancellationRequested);
        first.Finish(Replace(raw, "one", "ONE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var result = await polisher.CompleteAsync(appended, CancellationToken.None);
        Assert.Single(requests.Items);
        Assert.Equal("ONE two three four " + Words(0, 100), result.Text);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(100, result.UnprocessedWords);
    }

    [Fact]
    public async Task Complete_freezes_immediately_without_final_call_and_late_result_is_immutable()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        var task = polisher.CompleteAsync(raw + "tail", CancellationToken.None);
        Assert.True(task.IsCompletedSuccessfully);
        var result = await task;
        var progress = polisher.Progress;
        Assert.Equal(0, progress.PendingBlocks);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(5, result.UnprocessedWords);
        await Until(() => first.Token.IsCancellationRequested);
        first.Finish(Replace(first.Raw, "one", "ONE"));
        await first.Returned.Task;
        polisher.Update(raw + Words(0, 100));
        Assert.Same(task, polisher.CompleteAsync("different", CancellationToken.None));
        Assert.Equal(progress, polisher.Progress);
        Assert.Equal(raw + "tail", result.Text);
        Assert.Single(requests.Items);
    }

    [Fact]
    public async Task Completion_cancellation_freezes_and_cancels_without_waiting()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4);
        var raw = Words(0, 4);
        polisher.Update(raw);
        var first = await requests.At(0);
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var task = polisher.CompleteAsync(raw, cts.Token);
        Assert.True(task.IsCanceled);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => task);
        await Until(() => first.Token.IsCancellationRequested);
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
        Assert.Equal(0, polisher.Progress.PendingBlocks);
        first.Finish(Unchanged(raw));
    }

    [Fact]
    public async Task Dispose_is_prompt_even_if_cancellation_callback_blocks()
    {
        var requests = new Requests();
        var polisher = new DictationPolisher(requests.Refine, targetWords: 4);
        polisher.Update(Words(0, 4));
        var call = await requests.At(0);
        using var entered = new ManualResetEventSlim();
        using var release = new ManualResetEventSlim();
        using var registration = call.Token.Register(() =>
        {
            entered.Set();
            release.Wait(TimeSpan.FromSeconds(5));
        });
        try
        {
            var dispose = polisher.DisposeAsync();
            Assert.True(dispose.IsCompletedSuccessfully);
            await Until(() => entered.IsSet);
            Assert.Equal(0, polisher.Progress.FallbackBlocks);
            Assert.Throws<ObjectDisposedException>(() => polisher.Update("anything"));
        }
        finally
        {
            release.Set();
            call.Finish(Unchanged(call.Raw));
        }
    }

    [Fact]
    public async Task Timeout_warns_once_and_holds_physical_slot_until_underlying_really_returns()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        var log = new RecordingLog();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4,
            overlapWords: 1, timeProvider: clock, log: log);
        var raw = Words(0, 4);
        polisher.Update(raw);
        var first = await requests.At(0);
        await Until(() => clock.TimerCount > 0);
        clock.Advance(TimeSpan.FromSeconds(24));
        Assert.Equal(0, polisher.Progress.FallbackBlocks);
        Assert.False(first.Token.IsCancellationRequested);
        clock.Advance(TimeSpan.FromSeconds(1));
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        Assert.Contains("timed out", polisher.Progress.LastFailure);
        Assert.Single(log.Lines);
        Assert.Contains("Warn: dictation AI: fallback-block=1; reason=Refinement request timed out", log.All);
        await Until(() => first.Token.IsCancellationRequested);
        raw += Words(4, 20);
        polisher.Update(raw);
        clock.Advance(TimeSpan.FromMinutes(1));
        polisher.Update(raw);
        Assert.Single(requests.Items);
        Assert.Equal(0, polisher.Progress.PendingBlocks);
        raw += Words(24, 4);
        polisher.Update(raw);
        first.Finish(Replace(first.Raw, "w0", "LATE"));
        var second = await requests.At(1);
        Assert.Equal(Words(24, 4), second.Raw);
        Assert.DoesNotContain("LATE", polisher.Progress.Text);
        Assert.Equal(1, requests.Peak);
        second.Finish(Unchanged(second.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        Assert.Equal(2, polisher.Progress.FallbackBlocks); // Timeout plus discarded backlog.
        Assert.Equal(2, log.Lines.Count);
        Assert.Contains("exceeded the rolling window", log.Lines[1]);
    }

    [Fact]
    public async Task Response_after_old_eight_second_deadline_is_accepted_without_adding_a_final_wait()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4,
            overlapWords: 1, timeProvider: clock);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        await Until(() => clock.TimerCount > 0);
        clock.Advance(TimeSpan.FromSeconds(19));
        first.Finish(Replace(raw, "one", "ONE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var appended = raw + "five six seven ";
        polisher.Update(appended);
        var pending = await requests.At(1);
        polisher.StopScheduling();
        var completion = polisher.CompleteAsync(appended + "tail", CancellationToken.None);
        Assert.True(completion.IsCompletedSuccessfully);
        var result = await completion;
        Assert.Equal("ONE two three four five six seven tail", result.Text);
        Assert.Equal(1, result.SuccessfulBlocks);
        Assert.Equal(0, result.FallbackBlocks);
        await Until(() => pending.Token.IsCancellationRequested);
        pending.Finish(Replace(pending.Raw, "seven", "SEVEN"));
        await pending.Returned.Task;
        Assert.Equal(result.Text, polisher.Progress.Text);
        Assert.Equal(2, requests.Items.Count);
    }

    [Fact]
    public async Task Too_old_low_volume_backlog_is_explicit_raw_fallback_not_an_editable_snapshot()
    {
        var clock = new ManualClock();
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, timeProvider: clock);
        const string stale = "old speech ";
        polisher.Update(stale);
        clock.Advance(TimeSpan.FromSeconds(31));
        polisher.Update(stale + "fresh speech ");
        var call = await requests.At(0);
        Assert.Equal("fresh speech ", call.Raw);
        Assert.Equal(stale, call.Context);
        Assert.Equal(1, polisher.Progress.FallbackBlocks);
        Assert.Contains("too old", polisher.Progress.LastFailure);
        call.Finish(Replace(call.Raw, "fresh", "FRESH"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var result = await polisher.CompleteAsync(stale + "fresh speech ", CancellationToken.None);
        Assert.Equal("old speech FRESH speech ", result.Text);
        Assert.Equal(2, result.UnprocessedWords);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task Service_failure_or_unexpected_cancellation_has_explicit_sanitized_reason(bool cancel)
    {
        const string privateText = "private transcript contents four ";
        var requests = new Requests();
        var log = new RecordingLog();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, log: log);
        polisher.Update(privateText);
        var call = await requests.At(0);
        call.Completion.SetException(cancel
            ? new OperationCanceledException(privateText)
            : new InvalidOperationException(privateText));
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        Assert.Equal(privateText, polisher.Progress.Text);
        Assert.Equal(0, polisher.Progress.SuccessfulBlocks);
        Assert.NotNull(polisher.Progress.LastFailure);
        Assert.DoesNotContain(privateText, polisher.Progress.LastFailure);
        Assert.Single(log.Lines);
        Assert.Contains(polisher.Progress.LastFailure!, log.All);
        Assert.DoesNotContain(privateText, log.All);
        var result = await polisher.CompleteAsync(privateText + "tail", CancellationToken.None);
        Assert.Equal(1, result.FallbackBlocks);
    }

    [Fact]
    public async Task Changed_raw_prefix_discards_edits_and_active_response_with_one_warning()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4, overlapWords: 1);
        const string raw = "one two three four ";
        polisher.Update(raw);
        var first = await requests.At(0);
        first.Finish(Replace(raw, "one", "ONE"));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        polisher.Update(raw + "five six seven ");
        var second = await requests.At(1);
        const string revised = "replaced original prefix now ";
        polisher.Update(revised);
        Assert.Equal(revised, polisher.Progress.Text);
        Assert.Equal(1, polisher.Progress.FallbackBlocks);
        Assert.Contains("prefix changed", polisher.Progress.LastFailure);
        await Until(() => second.Token.IsCancellationRequested);
        second.Finish(Replace(second.Raw, "four", "FOUR"));
        polisher.Update(revised + Words(0, 100));
        var result = await polisher.CompleteAsync(revised + Words(0, 100), CancellationToken.None);
        Assert.Equal(revised + Words(0, 100), result.Text);
        Assert.Equal(1, result.FallbackBlocks);
        Assert.Equal(2, requests.Items.Count);
    }

    [Fact]
    public async Task Twenty_minutes_keep_requests_and_original_context_bounded()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine);
        var raw = "";
        for (var batch = 0; batch < 40; batch++)
        {
            raw += Words(batch * 75, 75);
            polisher.Update(raw);
            var call = await requests.At(batch);
            Assert.InRange(WordCount(call.Raw), 1, 75);
            Assert.InRange(call.Raw.Length, 1, 4000);
            Assert.InRange(WordCount(call.Context), 0, 150);
            Assert.InRange(call.Context.Length, 0, 8000);
            Assert.DoesNotContain("EDITED", call.Context);
            call.Finish(Replace(call.Raw, $"w{batch * 75} ", $"EDITED{batch} "));
            await Until(() => polisher.Progress.SuccessfulBlocks == batch + 1);
        }
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Equal(40, result.SuccessfulBlocks);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(0, result.UnprocessedWords);
        Assert.Equal(40, requests.Items.Count);
        Assert.Equal(1, requests.Peak);
        Assert.Equal(40, result.Text.Split("EDITED").Length - 1);
    }

    [Fact]
    public async Task Huge_unicode_token_is_raw_never_fragmented_and_context_respects_character_limit()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine, targetWords: 4);
        var huge = string.Concat(Enumerable.Repeat("\U0001F600", 6500));
        polisher.Update(huge);
        Assert.Empty(requests.Items);
        Assert.Equal(1, polisher.Progress.FallbackBlocks);
        polisher.Update(huge + "\U0001F600");
        Assert.Empty(requests.Items);
        var raw = huge + "\U0001F600 " + Words(0, 4);
        polisher.Update(raw);
        var call = await requests.At(0);
        Assert.DoesNotContain("\U0001F600", call.Raw);
        Assert.DoesNotContain("\U0001F600", call.Context);
        Assert.InRange(call.Context.Length, 0, 8000);
        call.Finish(Unchanged(call.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Equal(raw, result.Text);
        Assert.True(result.FallbackBlocks >= 1);
    }

    [Fact]
    public async Task Long_tokens_and_whitespace_respect_character_caps_and_are_not_silently_lost()
    {
        var requests = new Requests();
        await using var polisher = new DictationPolisher(requests.Refine);
        var raw = string.Join(' ', Enumerable.Range(0, 20).Select(i => new string((char)('a' + i), 1000)))
            + new string(' ', 9000) + "last token ";
        polisher.Update(raw);
        var call = await requests.At(0);
        Assert.Equal("last token ", call.Raw);
        Assert.InRange(call.Context.Length, 0, 8000);
        Assert.Equal(1, polisher.Progress.FallbackBlocks);
        call.Finish(Unchanged(call.Raw));
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Equal(raw, result.Text);
        Assert.Equal(20, result.UnprocessedWords);
    }

    public static IEnumerable<object[]> InvalidRefinements()
    {
        yield return [new DictationRefinement("unanchored", [])];
        yield return [new DictationRefinement("one two three four ", [new("absent", "new")])];
        yield return [new DictationRefinement("ONE two three four ", [new("one", "ONE"), new("one two", "X")])];
        yield return [new DictationRefinement("ONE two three four ", [new("one", "DIFFERENT")])];
        yield return [new DictationRefinement("one two three four ", [new("", "x")])];
        yield return [new DictationRefinement("one two three four ", null!)];
        yield return [new DictationRefinement(null!, [])];
        yield return [new DictationRefinement("one\u0000 two three four ", [new("one", "one\u0000")])];
        yield return [new DictationRefinement("\uD800 two three four ", [new("one", "\uD800")])];
        yield return [new DictationRefinement("\uDC00 two three four ", [new("one", "\uDC00")])];
        yield return [new DictationRefinement(new string('x', 8001), [])];
        yield return [new DictationRefinement("one two three four ", [null!])];
    }

    [Theory]
    [MemberData(nameof(InvalidRefinements))]
    public async Task Invalid_provenance_never_changes_raw_and_reports_failure(DictationRefinement response)
    {
        const string raw = "one two three four ";
        var log = new RecordingLog();
        await using var polisher = new DictationPolisher((_, _, _) => Task.FromResult(response),
            targetWords: 4, log: log);
        polisher.Update(raw);
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        var result = await polisher.CompleteAsync(raw, CancellationToken.None);
        Assert.Equal(raw, result.Text);
        Assert.Equal(0, result.SuccessfulBlocks);
        Assert.Contains("provenance", result.LastFailure);
        Assert.Single(log.Lines);
        Assert.Contains("provenance", log.All);
        Assert.DoesNotContain(raw, log.All);
    }

    [Fact]
    public async Task Original_must_be_unique_in_original_request_including_overlapping_occurrences()
    {
        const string raw = "aaa one two three ";
        await using var polisher = new DictationPolisher((_, _, _) =>
            Task.FromResult(new DictationRefinement("Xa one two three ", [new("aa", "X")])), targetWords: 4);
        polisher.Update(raw);
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        Assert.Equal(raw, polisher.Progress.Text);
    }

    [Fact]
    public async Task Edits_are_located_in_original_not_in_previous_replacements_and_may_arrive_unsorted()
    {
        const string raw = "alpha beta gamma delta ";
        await using var polisher = new DictationPolisher((_, _, _) => Task.FromResult(
            new DictationRefinement("beta BETA gamma delta ", [new("beta", "BETA"), new("alpha", "beta")])),
            targetWords: 4);
        polisher.Update(raw);
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        Assert.Equal("beta BETA gamma delta ", polisher.Progress.Text);
    }

    [Theory]
    [InlineData("one um two three ", "um ", "", "one two three ")]
    [InlineData("one two two three ", "two two", "two", "one two three ")]
    [InlineData("one \tuh\r\n two three ", "uh\r\n ", "", "one \ttwo three ")]
    [InlineData("one \U0001F600 two three ", "\U0001F600", "smile", "one smile two three ")]
    public async Task Exact_whitespace_and_unicode_edits_are_preserved(string raw, string original,
        string replacement, string expected)
    {
        await using var polisher = new DictationPolisher((_, _, _) => Task.FromResult(
            new DictationRefinement(expected, [new(original, replacement)])), targetWords: 4);
        polisher.Update(raw);
        await Until(() => polisher.Progress.SuccessfulBlocks == 1);
        Assert.Equal(expected, polisher.Progress.Text);
    }

    [Fact]
    public async Task Filler_deletion_cannot_join_neighboring_words()
    {
        const string raw = "one um two three ";
        await using var polisher = new DictationPolisher((_, _, _) => Task.FromResult(
            new DictationRefinement("onetwo three ", [new(" um ", "")])), targetWords: 4);
        polisher.Update(raw);
        await Until(() => polisher.Progress.FallbackBlocks == 1);
        Assert.Equal(raw, polisher.Progress.Text);
    }

    [Fact]
    public async Task Empty_session_or_normal_unprocessed_tail_is_not_a_failure()
    {
        var log = new RecordingLog();
        await using var empty = new DictationPolisher((_, _, _) => throw new InvalidOperationException(), log: log);
        var emptyResult = await empty.CompleteAsync("", CancellationToken.None);
        Assert.Equal("", emptyResult.Text);
        Assert.Equal(0, emptyResult.UnprocessedWords);
        await using var tail = new DictationPolisher((_, _, _) => throw new InvalidOperationException(), log: log);
        tail.Update("normal raw tail");
        var result = await tail.CompleteAsync("normal raw tail", CancellationToken.None);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Equal(3, result.UnprocessedWords);
        Assert.Null(result.LastFailure);
        Assert.Empty(log.Lines);
    }

    private static string Words(int start, int count) =>
        string.Join(' ', Enumerable.Range(start, count).Select(i => $"w{i}")) + " ";
    private static int WordCount(string text) =>
        text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;
    private static DictationRefinement Unchanged(string raw) => new(raw, []);
    private static DictationRefinement Replace(string raw, string original, string replacement) =>
        new(raw.Replace(original, replacement, StringComparison.Ordinal), [new(original, replacement)]);
    private static async Task Until(Func<bool> predicate)
    {
        var deadline = Environment.TickCount64 + 5000;
        while (!predicate())
        {
            Assert.True(Environment.TickCount64 < deadline, "Background operation did not settle.");
            await Task.Delay(5);
        }
    }

    private sealed class Request(string raw, string context, CancellationToken token)
    {
        public string Raw { get; } = raw;
        public string Context { get; } = context;
        public CancellationToken Token { get; } = token;
        public TaskCompletionSource<DictationRefinement> Completion { get; } =
            new(TaskCreationOptions.RunContinuationsAsynchronously);
        public TaskCompletionSource Returned { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public void Finish(DictationRefinement refinement) => Completion.SetResult(refinement);
    }

    private sealed class Requests
    {
        public ConcurrentQueue<Request> Items { get; } = new();
        private int _active;
        private int _peak;
        public int Peak => Volatile.Read(ref _peak);
        public async Task<DictationRefinement> Refine(string raw, string context, CancellationToken ct)
        {
            var active = Interlocked.Increment(ref _active);
            Interlocked.Exchange(ref _peak, Math.Max(active, Peak));
            var request = new Request(raw, context, ct);
            Items.Enqueue(request);
            try { return await request.Completion.Task; }
            finally
            {
                Interlocked.Decrement(ref _active);
                request.Returned.TrySetResult();
            }
        }
        public async Task<Request> At(int index)
        {
            await Until(() => Items.Count > index);
            return Items.ToArray()[index];
        }
    }

    private sealed class ManualClock : TimeProvider
    {
        private readonly object _gate = new();
        private readonly List<ManualTimer> _timers = [];
        private long _ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
        public override long GetTimestamp() { lock (_gate) return _ticks; }
        public override DateTimeOffset GetUtcNow() => DateTimeOffset.UnixEpoch.AddTicks(GetTimestamp());
        public int TimerCount { get { lock (_gate) return _timers.Count(t => !t.Disposed); } }
        public void Advance(TimeSpan elapsed)
        {
            List<ManualTimer> due;
            lock (_gate)
            {
                _ticks += elapsed.Ticks;
                due = _timers.Where(t => !t.Disposed && t.Due <= _ticks).ToList();
                foreach (var timer in due) timer.Due = long.MaxValue;
            }
            foreach (var timer in due) timer.Callback(timer.State);
        }
        public override ITimer CreateTimer(TimerCallback callback, object? state, TimeSpan dueTime, TimeSpan period)
        {
            lock (_gate)
            {
                var timer = new ManualTimer(this, callback, state);
                timer.Change(dueTime, period);
                _timers.Add(timer);
                return timer;
            }
        }
        private sealed class ManualTimer(ManualClock clock, TimerCallback callback, object? state) : ITimer
        {
            public TimerCallback Callback { get; } = callback;
            public object? State { get; } = state;
            public long Due { get; set; }
            public bool Disposed { get; private set; }
            public bool Change(TimeSpan dueTime, TimeSpan period)
            {
                lock (clock._gate)
                {
                    if (Disposed) return false;
                    Due = dueTime == Timeout.InfiniteTimeSpan ? long.MaxValue : clock._ticks + dueTime.Ticks;
                    return true;
                }
            }
            public void Dispose() { lock (clock._gate) Disposed = true; }
            public ValueTask DisposeAsync() { Dispose(); return ValueTask.CompletedTask; }
        }
    }
}
