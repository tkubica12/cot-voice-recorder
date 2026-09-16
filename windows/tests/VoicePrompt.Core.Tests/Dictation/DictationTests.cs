using System.Buffers.Binary;
using System.Diagnostics;
using System.Net;
using System.Text;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Settings;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public class ChunkerTests
{
    internal static byte[] Pcm(int milliseconds, short sample = 1200)
    {
        var bytes = new byte[PcmChunker.BytesPerSecond / 1000 * milliseconds];
        for (var i = 0; i < bytes.Length; i += 2)
            BinaryPrimitives.WriteInt16LittleEndian(bytes.AsSpan(i), sample);
        return bytes;
    }

    [Fact]
    public void Wave_header_matches_actual_audio()
    {
        var pcm = Pcm(100);
        var wav = PcmChunker.ToWav(pcm);
        Assert.Equal("RIFF", Encoding.ASCII.GetString(wav, 0, 4));
        Assert.Equal("WAVEfmt ", Encoding.ASCII.GetString(wav, 8, 8));
        Assert.Equal(wav.Length - 8, BinaryPrimitives.ReadInt32LittleEndian(wav.AsSpan(4)));
        Assert.Equal(16000, BinaryPrimitives.ReadInt32LittleEndian(wav.AsSpan(24)));
        Assert.Equal(32000, BinaryPrimitives.ReadInt32LittleEndian(wav.AsSpan(28)));
        Assert.Equal(1, BinaryPrimitives.ReadInt16LittleEndian(wav.AsSpan(22)));
        Assert.Equal(16, BinaryPrimitives.ReadInt16LittleEndian(wav.AsSpan(34)));
        Assert.Equal(pcm.Length, BinaryPrimitives.ReadInt32LittleEndian(wav.AsSpan(40)));
        Assert.Equal(pcm, wav[44..]);
    }

    [Fact]
    public void Five_minutes_of_silence_never_uploads()
    {
        var chunker = new PcmChunker();
        Assert.Empty(chunker.Append(Pcm(300000, 0)));
        Assert.Empty(chunker.Flush());
    }

    [Fact]
    public void Speech_flushes_immediately_without_waiting_for_minimum_chunk()
    {
        var chunker = new PcmChunker();
        Assert.Empty(chunker.Append(Pcm(300)));
        var chunk = Assert.Single(chunker.Flush());
        Assert.Equal(44 + 9600, chunk.Wav.Length);
        Assert.False(chunk.OverlapsPrevious);
        Assert.Empty(chunker.Flush());
    }

    [Fact]
    public void Pause_cuts_at_two_seconds_without_overlap()
    {
        var chunker = new PcmChunker();
        Assert.Empty(chunker.Append(Pcm(1600)));
        var first = Assert.Single(chunker.Append(Pcm(400, 0)));
        Assert.Equal(64044, first.Wav.Length);
        chunker.Append(Pcm(300));
        Assert.False(Assert.Single(chunker.Flush()).OverlapsPrevious);
    }

    [Fact]
    public void Continuous_speech_is_bounded_and_preserves_exact_overlap()
    {
        var pcm = Pcm(14000);
        for (var i = 0; i < pcm.Length; i += 2)
            BinaryPrimitives.WriteInt16LittleEndian(pcm.AsSpan(i), (short)(1000 + i % 20000));
        var chunker = new PcmChunker();
        var chunks = chunker.Append(pcm).Concat(chunker.Flush()).ToArray();
        Assert.Equal(3, chunks.Length);
        Assert.False(chunks[0].OverlapsPrevious);
        Assert.All(chunks.Skip(1), c => Assert.True(c.OverlapsPrevious));
        Assert.All(chunks, c => Assert.True(c.Wav.Length <= 192044));
        var overlapBytes = PcmChunker.BytesPerSecond * 600 / 1000;
        Assert.Equal(chunks[0].Wav[^overlapBytes..], chunks[1].Wav[44..(44 + overlapBytes)]);
        var reconstructed = chunks[0].Wav[44..].Concat(
            chunks.Skip(1).SelectMany(c => c.Wav[(44 + overlapBytes)..])).ToArray();
        Assert.Equal(pcm, reconstructed);
    }

    [Fact]
    public void Exact_boundary_does_not_upload_overlap_only_tail()
    {
        var chunker = new PcmChunker();
        Assert.Single(chunker.Append(Pcm(6000)));
        Assert.Empty(chunker.Flush());
    }

    [Fact]
    public void Long_pause_after_hard_boundary_drops_overlap_metadata()
    {
        var chunker = new PcmChunker();
        Assert.Single(chunker.Append(Pcm(6000)));
        Assert.Empty(chunker.Append(Pcm(5000, 0)));
        Assert.Empty(chunker.Append(Pcm(300)));
        Assert.False(Assert.Single(chunker.Flush()).OverlapsPrevious);
    }

    [Fact]
    public void Arbitrary_capture_buffer_boundaries_do_not_lose_samples()
    {
        var pcm = Pcm(111);
        var chunker = new PcmChunker();
        for (var offset = 0; offset < pcm.Length; offset += 37)
            Assert.Empty(chunker.Append(pcm.AsSpan(offset, Math.Min(37, pcm.Length - offset))));
        Assert.Equal(pcm, Assert.Single(chunker.Flush()).Wav[44..]);
    }

    [Fact]
    public void Incomplete_sample_is_an_explicit_error()
    {
        var chunker = new PcmChunker();
        chunker.Append(new byte[] { 1 });
        Assert.Throws<InvalidDataException>(() => chunker.Flush());
    }
}

public class SessionTests
{
    [Fact]
    public async Task Starts_during_capture_limits_concurrency_and_orders_out_of_order_results()
    {
        var pending = new[] { Signal(), Signal(), Signal() };
        var calls = 0;
        await using var session = new DictationSession((_, _) => pending[Interlocked.Increment(ref calls) - 1].Task);
        session.Add(new AudioChunk(new byte[2], false));
        session.Add(new AudioChunk(new byte[2], false));
        session.Add(new AudioChunk(new byte[2], false));
        Assert.Equal(2, calls);
        var finish = session.CompleteAsync();
        pending[1].SetResult("second");
        await WaitUntilAsync(() => Volatile.Read(ref calls) == 3);
        pending[2].SetResult("third");
        Assert.False(finish.IsCompleted);
        pending[0].SetResult("first");
        Assert.Equal("first second third", await finish);
    }

    [Fact]
    public async Task Stop_does_not_retranscribe_already_completed_audio()
    {
        var calls = 0;
        await using var session = new DictationSession((_, _) =>
        {
            calls++;
            return Task.FromResult("ready");
        });
        session.Add(new AudioChunk(new byte[2], false));
        var watch = Stopwatch.StartNew();
        var task = session.CompleteAsync();
        Assert.True(task.IsCompletedSuccessfully);
        Assert.Equal("ready", await task);
        Assert.Equal(1, calls);
        Assert.True(watch.Elapsed < TimeSpan.FromSeconds(1));
    }

    [Fact]
    public async Task Backlog_is_bounded_and_cancellation_reaches_inflight_requests()
    {
        var cancelled = 0;
        await using var session = new DictationSession(async (_, ct) =>
        {
            try { await Task.Delay(Timeout.Infinite, ct); }
            catch (OperationCanceledException) { Interlocked.Increment(ref cancelled); throw; }
            return "";
        });
        for (var i = 0; i < DictationSession.MaxPendingChunks; i++)
            session.Add(new AudioChunk(new byte[2], false));
        Assert.Throws<InvalidOperationException>(() => session.Add(new AudioChunk(new byte[2], false)));
        session.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => session.CompleteAsync());
        Assert.Equal(2, cancelled);
    }

    [Fact]
    public async Task A_failed_chunk_never_returns_successful_partial_text()
    {
        var failed = Signal();
        var count = 0;
        await using var session = new DictationSession((_, _) =>
            count++ == 0 ? Task.FromResult("partial") : failed.Task);
        session.Add(new AudioChunk(new byte[2], false));
        session.Add(new AudioChunk(new byte[2], false));
        failed.SetException(new IOException("offline"));
        await Assert.ThrowsAsync<IOException>(() => session.CompleteAsync());
        Assert.NotNull(session.Failure);
    }

    [Theory]
    [InlineData("Deploy Kubernetes.", "kubernetes functions now", true, "Deploy Kubernetes. functions now")]
    [InlineData("say go", "go now", true, "say go now")]
    [InlineData("send it to", "to the editor", true, "send it to the editor")]
    [InlineData("release with that.", "that and include notes.", true, "release with that. and include notes.")]
    [InlineData("the release notes", "release notes are ready", true, "the release notes are ready")]
    [InlineData("chci to", "to poslat", true, "chci to poslat")]
    [InlineData("say go", "go now", false, "say go go now")]
    [InlineData("I know that", "that is true", false, "I know that that is true")]
    [InlineData("very very", "very good", true, "very very good")]
    [InlineData("write notes", "nodes there", true, "write notes nodes there")]
    [InlineData("...", "...", true, "... ...")]
    [InlineData("very very", "very very good", false, "very very very very good")]
    [InlineData("use Azure Functions", "azure functions please", true, "use Azure Functions please")]
    public void Deduplication_only_applies_to_actual_overlap(string a, string b, bool overlap, string expected) =>
        Assert.Equal(expected, DictationSession.Stitch(new[] { (a, false), (b, overlap) }));

    [Fact]
    public void An_empty_chunk_breaks_overlap_matching_with_earlier_text()
    {
        Assert.Equal("use Kubernetes Kubernetes again", DictationSession.Stitch(
            [("use Kubernetes", false), ("", true), ("Kubernetes again", true)]));
    }

    [Fact]
    public void Overlap_never_matches_more_than_the_immediately_previous_chunk()
    {
        Assert.Equal("alpha beta alpha beta gamma", DictationSession.Stitch(
            [("alpha beta", false), ("beta", true), ("alpha beta gamma", true)]));
    }

    private static TaskCompletionSource<string> Signal() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static async Task WaitUntilAsync(Func<bool> done)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(3));
        while (!done()) await Task.Delay(5, timeout.Token);
    }
}

public class DeliveryTests
{
    private sealed class Desktop : IDictationDesktop
    {
        public bool IsTargetUnchanged { get; set; } = true;
        public bool AreModifiersReleased { get; set; } = true;
        public bool AllowInput { get; set; } = true;
        public int Pastes { get; private set; }
        public bool TrySendPaste() { Pastes++; return AllowInput; }
    }
    private sealed class Writer : IClipboardWriter
    {
        public string? Text { get; private set; }
        public bool Fail { get; set; }
        public void SetText(string text)
        {
            if (Fail) throw new IOException("busy");
            Text = text;
        }
    }

    [Fact]
    public async Task Copies_unicode_and_sends_one_paste()
    {
        var desktop = new Desktop();
        var writer = new Writer();
        Assert.Equal(DictationDeliveryResult.PasteSent, await DictationDelivery.DeliverAsync(
            "Příliš žluťoučký kůň", desktop, new ClipboardCopier(writer), CancellationToken.None));
        Assert.Equal("Příliš žluťoučký kůň", writer.Text);
        Assert.Equal(1, desktop.Pastes);
    }

    [Fact]
    public async Task Focus_change_copies_but_never_injects_input()
    {
        var desktop = new Desktop { IsTargetUnchanged = false };
        var writer = new Writer();
        Assert.Equal(DictationDeliveryResult.CopiedOnly, await DictationDelivery.DeliverAsync(
            "hello", desktop, new ClipboardCopier(writer), CancellationToken.None));
        Assert.Equal("hello", writer.Text);
        Assert.Equal(0, desktop.Pastes);
    }

    [Fact]
    public async Task Waits_for_shortcut_release_then_rechecks_focus()
    {
        var desktop = new Desktop { AreModifiersReleased = false };
        Assert.Equal(DictationDeliveryResult.CopiedOnly, await DictationDelivery.DeliverAsync(
            "hello", desktop, new ClipboardCopier(new Writer()), CancellationToken.None,
            (_, _) => { desktop.IsTargetUnchanged = false; return Task.CompletedTask; }));
        Assert.Equal(0, desktop.Pastes);
    }

    [Fact]
    public async Task Held_modifiers_have_a_bounded_wait()
    {
        var desktop = new Desktop { AreModifiersReleased = false };
        var waits = 0;
        Assert.Equal(DictationDeliveryResult.CopiedOnly, await DictationDelivery.DeliverAsync(
            "hello", desktop, new ClipboardCopier(new Writer()), CancellationToken.None,
            (_, _) => { waits++; return Task.CompletedTask; }));
        Assert.Equal(30, waits);
        Assert.Equal(0, desktop.Pastes);
    }

    [Fact]
    public async Task Clipboard_failure_prevents_paste()
    {
        var desktop = new Desktop();
        Assert.Equal(DictationDeliveryResult.ClipboardFailed, await DictationDelivery.DeliverAsync(
            "hello", desktop, new ClipboardCopier(new Writer { Fail = true }, maxAttempts: 1), CancellationToken.None));
        Assert.Equal(0, desktop.Pastes);
    }

    [Fact]
    public async Task Cancelled_delivery_does_not_copy_or_paste()
    {
        var desktop = new Desktop();
        var writer = new Writer();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => DictationDelivery.DeliverAsync(
            "hello", desktop, new ClipboardCopier(writer), new CancellationToken(true)));
        Assert.Null(writer.Text);
        Assert.Equal(0, desktop.Pastes);
    }

    [Fact]
    public async Task Input_rejection_reports_clipboard_fallback()
    {
        Assert.Equal(DictationDeliveryResult.CopiedOnly, await DictationDelivery.DeliverAsync(
            "hello", new Desktop { AllowInput = false }, new ClipboardCopier(new Writer()), CancellationToken.None));
    }
}

public class DictationApiTests
{
    [Fact]
    public async Task Sends_raw_wav_with_language_and_auth_refresh_replays_body()
    {
        var wav = PcmChunker.ToWav(ChunkerTests.Pcm(100));
        var observed = new List<byte[]>();
        var handler = new FakeHttpMessageHandler();
        handler.Enqueue(request =>
        {
            observed.Add(request.Content!.ReadAsByteArrayAsync().Result);
            Assert.Equal("audio/wav", request.Content.Headers.ContentType!.MediaType);
            return new HttpResponseMessage(HttpStatusCode.Unauthorized);
        });
        handler.Enqueue(request =>
        {
            observed.Add(request.Content!.ReadAsByteArrayAsync().Result);
            return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("{\"text\":\"hello\"}") };
        });
        var credentials = new FakeCredentials();
        using var http = new HttpClient(handler) { BaseAddress = new Uri("https://example.test") };
        var client = new ApiClient(http, credentials);
        Assert.Equal("hello", await client.TranscribeDictationAsync(wav, "auto", CancellationToken.None));
        Assert.All(observed, body => Assert.Equal(wav, body));
        Assert.Equal(2, observed.Count);
        Assert.Contains("language=auto", handler.Requests[0].RequestUri!.Query);
    }

    [Fact]
    public async Task Missing_text_is_not_silent_success()
    {
        using var http = new HttpClient(new FakeHttpMessageHandler().EnqueueJson(HttpStatusCode.OK, "{}"))
        { BaseAddress = new Uri("https://example.test") };
        await Assert.ThrowsAsync<ApiException>(() => new ApiClient(http, new FakeCredentials())
            .TranscribeDictationAsync(new byte[2], "auto", CancellationToken.None));
    }

    [Fact]
    public async Task Backend_can_change_after_first_request_without_mutating_HttpClient()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.OK, "{\"text\":\"one\"}")
            .EnqueueJson(HttpStatusCode.OK, "{\"text\":\"two\"}");
        using var http = new HttpClient(handler);
        var url = new Uri("https://one.test");
        var api = new ApiClient(http, new FakeCredentials(), baseUrl: () => url);
        await api.TranscribeDictationAsync(new byte[2], "cs", CancellationToken.None);
        url = new Uri("https://two.test");
        await api.TranscribeDictationAsync(new byte[2], "en", CancellationToken.None);
        Assert.Equal("one.test", handler.Requests[0].RequestUri!.Host);
        Assert.Equal("two.test", handler.Requests[1].RequestUri!.Host);
    }

    [Fact]
    public void Settings_roundtrip_preserves_dictation_and_old_settings_get_defaults()
    {
        var fs = new FakeFileSystem();
        var store = new SettingsStore("settings.json", fs);
        fs.Seed("settings.json", "{\"auto_start\":true}");
        var settings = store.Load();
        Assert.True(settings.DictationEnabled);
        Assert.Equal("Ctrl+Alt+Space", settings.DictationShortcut);
        Assert.Equal("Ctrl+Alt+Shift+Space", settings.DictationToggleShortcut);
        settings.DictationShortcut = "Ctrl+Shift+D";
        settings.DictationToggleShortcut = "Ctrl+Alt+D";
        settings.DictationLanguage = "cs";
        store.Save(settings.Clone());
        Assert.Equal("Ctrl+Shift+D", store.Load().DictationShortcut);
        Assert.Equal("cs", store.Load().DictationLanguage);
        Assert.Equal("Ctrl+Alt+D", store.Load().DictationToggleShortcut);
    }
}
