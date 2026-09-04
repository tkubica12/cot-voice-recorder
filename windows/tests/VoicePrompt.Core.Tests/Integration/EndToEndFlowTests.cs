using System.Net;
using VoicePrompt.Core;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;
using VoicePrompt.Core.Realtime;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Integration;

/// <summary>
/// End-to-end wiring across the real <see cref="RealtimeClient"/>, <see cref="ApiClient"/>,
/// <see cref="TranscriptCoordinator"/>, <see cref="HistoryStore"/> and
/// <see cref="ClipboardCopier"/>, driven by fakes at the process edges only (HTTP handler,
/// WebSocket transport, clipboard, clock, file system). No live Google or Azure calls.
/// </summary>
public class EndToEndFlowTests
{
    private const string TranscriptBody = "Připomeň mi zítra ráno zavolat doktorovi ohledně výsledků.";

    private const string TranscriptJson = $$"""
    {
      "transcript_id": "t-1",
      "recording_id": "r-1",
      "body": "{{TranscriptBody}}",
      "preview": "Připomeň mi zítra ráno…",
      "language": "cs",
      "refine_model": "gpt-5.6-luna",
      "completed_at": "2026-09-03T13:53:10Z",
      "expires_at": "2026-09-05T13:53:10Z",
      "character_count": 58
    }
    """;

    private const string NegotiateJson = """
    {
      "url": "wss://wps.example.test/client/hubs/transcripts?access_token=abc",
      "hub": "transcripts",
      "group": "user",
      "expires_at": "2126-09-03T14:52:00Z"
    }
    """;

    private static string EventFrame(string eventId) => $$$"""
    {"type":"message","from":"server","dataType":"json","data":{"event":"transcript.completed","event_id":"{{{eventId}}}","transcript_id":"t-1","recording_id":"r-1","completed_at":"2026-09-03T13:53:10Z","preview":"Připomeň mi zítra ráno…"}}
    """;

    private sealed class Harness : IAsyncDisposable
    {
        public FakeHttpMessageHandler Handler { get; } = new();
        public FakeCredentials Credentials { get; } = new();
        public FakeClipboardWriter Clipboard { get; } = new();
        public RecordingNotifier Notifier { get; } = new();
        public RecordingLog Log { get; } = new();
        public FakeClock Clock { get; } = new(new DateTimeOffset(2026, 9, 3, 14, 0, 0, TimeSpan.Zero));
        public FakeFileSystem Fs { get; } = new();
        public bool Paused { get; set; }

        public HttpClient Http { get; private set; } = null!;
        public ApiClient Api { get; private set; } = null!;
        public HistoryStore History { get; private set; } = null!;
        public TranscriptCoordinator Coordinator { get; private set; } = null!;
        public RealtimeClient? Realtime { get; private set; }

        public Harness Build()
        {
            Http = new HttpClient(Handler) { BaseAddress = new Uri("https://api.example.test/") };
            Api = new ApiClient(
                Http, Credentials,
                new ApiRetryOptions { MaxRetries = 2, Backoff = _ => TimeSpan.Zero },
                (_, _) => Task.CompletedTask);
            History = new HistoryStore(@"C:\state\history.json", Fs, Clock);
            Coordinator = new TranscriptCoordinator(
                Api, History,
                new ClipboardCopier(Clipboard, 3, delay: (_, _) => Task.CompletedTask),
                Notifier, Clock, () => Paused, log: Log);
            return this;
        }

        public RealtimeClient WithRealtime(params FakeRealtimeSocket[] sockets)
        {
            Realtime = new RealtimeClient(
                new ApiRealtimeNegotiator(Api, "1.0.0-test"),
                new FakeRealtimeSocketFactory(sockets),
                Clock,
                Log,
                new ReconnectPolicy(TimeSpan.FromMilliseconds(1), TimeSpan.FromMilliseconds(5)),
                delay: (_, _) => Task.CompletedTask);
            Realtime.TranscriptCompleted += Coordinator.HandleAsync;
            return Realtime;
        }

        public async ValueTask DisposeAsync()
        {
            if (Realtime is not null)
            {
                await Realtime.DisposeAsync();
            }

            Http.Dispose();
        }
    }

    private static async Task WaitUntil(Func<bool> condition, int timeoutMs = 5000)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        while (DateTime.UtcNow < deadline)
        {
            if (condition())
            {
                return;
            }

            await Task.Delay(5);
        }

        throw new TimeoutException("condition was not met in time");
    }

    // ------------------------------------------------------------------ full path

    [Fact]
    public async Task Negotiate_then_event_then_fetch_then_clipboard_and_history()
    {
        await using var h = new Harness().Build();
        h.Handler
            .EnqueueJson(HttpStatusCode.OK, NegotiateJson)
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        var socket = new FakeRealtimeSocket(
            new[] { """{"type":"system","event":"connected","userId":"owner"}""", EventFrame("e-1") },
            holdOpenAfterFrames: true);
        var realtime = h.WithRealtime(socket);

        realtime.Start();
        await WaitUntil(() => h.Clipboard.LastText is not null);
        await realtime.StopAsync();

        // Clipboard received the full body.
        Assert.Equal(TranscriptBody, h.Clipboard.LastText);

        // History cached it locally.
        var cached = h.History.Latest();
        Assert.NotNull(cached);
        Assert.Equal("t-1", cached!.TranscriptId);
        Assert.Equal(TranscriptBody, cached.Body);
        Assert.Equal(58, cached.CharacterCount);

        // Exactly two backend calls: negotiate + the single transcript fetch (no polling).
        Assert.Equal(2, h.Handler.Requests.Count);
        Assert.Equal("/v1/realtime/negotiate", h.Handler.Requests[0].RequestUri!.AbsolutePath);
        Assert.Equal("/v1/transcripts/t-1", h.Handler.Requests[1].RequestUri!.AbsolutePath);
        Assert.All(h.Handler.AuthorizationValues, v => Assert.Equal("token-1", v));

        // A notification was shown with the preview only.
        var notification = Assert.Single(h.Notifier.Notifications);
        Assert.Equal("Transcript copied", notification.Title);
        Assert.DoesNotContain("výsledků", notification.Message);

        // Nothing private leaked into the log.
        Assert.DoesNotContain(TranscriptBody, h.Log.All);
        Assert.DoesNotContain("access_token=abc", h.Log.All);
    }

    [Fact]
    public async Task A_duplicate_event_id_is_handled_exactly_once()
    {
        await using var h = new Harness().Build();
        h.Handler
            .EnqueueJson(HttpStatusCode.OK, NegotiateJson)
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        var socket = new FakeRealtimeSocket(
            new[] { EventFrame("e-dup"), EventFrame("e-dup"), EventFrame("e-dup") },
            holdOpenAfterFrames: true);
        var realtime = h.WithRealtime(socket);

        var results = new List<TranscriptHandlingResult>();
        h.Coordinator.Handled += r => { lock (results) { results.Add(r); } };

        realtime.Start();
        await WaitUntil(() => results.Count == 3);
        await realtime.StopAsync();

        Assert.Equal(TranscriptHandlingResult.CopiedAndNotified, results[0]);
        Assert.Equal(TranscriptHandlingResult.Duplicate, results[1]);
        Assert.Equal(TranscriptHandlingResult.Duplicate, results[2]);

        // Only one transcript fetch happened.
        Assert.Equal(2, h.Handler.Requests.Count);
        Assert.Equal(1, h.Clipboard.Attempts);
        Assert.Single(h.History.All());
    }

    [Fact]
    public async Task Auth_refresh_on_401_is_transparent_to_the_end_to_end_flow()
    {
        await using var h = new Harness().Build();
        h.Handler
            .EnqueueJson(HttpStatusCode.OK, NegotiateJson)
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""")
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        var socket = new FakeRealtimeSocket(new[] { EventFrame("e-401") }, holdOpenAfterFrames: true);
        var realtime = h.WithRealtime(socket);

        realtime.Start();
        await WaitUntil(() => h.Clipboard.LastText is not null);
        await realtime.StopAsync();

        Assert.Equal(TranscriptBody, h.Clipboard.LastText);
        Assert.Equal(1, h.Credentials.RefreshCalls);
        Assert.Equal(new[] { "token-1", "token-1", "token-2" }, h.Handler.AuthorizationValues);
    }

    [Fact]
    public async Task A_real_google_refresh_supplies_the_retry_token_after_a_401()
    {
        // Exercises AuthManager -> AuthBackendCredentials -> ApiClient with no live Google.
        var clock = new FakeClock();
        var fs = new FakeFileSystem();
        var protector = new FakeSecretProtector();
        var store = new TokenStore(@"C:\state\tokens.bin", protector, fs);

        var staleIdToken = TestTokens.Jwt(clock.UtcNow.AddHours(1), "owner@example.com");
        var freshIdToken = TestTokens.Jwt(clock.UtcNow.AddHours(2), "owner@example.com");
        store.Save(new CachedTokens { RefreshToken = "refresh-1", IdToken = staleIdToken });

        var tokenClient = new FakeGoogleTokenClient();
        tokenClient.RefreshResponses.Enqueue(new TokenResponse { IdToken = freshIdToken });

        var auth = new AuthManager(
            TestTokens.Config(), tokenClient, store, new FakeBrowser(),
            new FakeLoopbackListenerFactory(() => ""), clock);

        var handler = new FakeHttpMessageHandler()
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""")
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        using var http = new HttpClient(handler) { BaseAddress = new Uri("https://api.example.test/") };
        var api = new ApiClient(
            http, new AuthBackendCredentials(auth),
            new ApiRetryOptions { MaxRetries = 1, Backoff = _ => TimeSpan.Zero },
            (_, _) => Task.CompletedTask);

        var transcript = await api.GetTranscriptAsync("t-1", CancellationToken.None);

        Assert.Equal(TranscriptBody, transcript.Body);
        Assert.Equal(1, tokenClient.RefreshCalls);
        Assert.Equal(staleIdToken, handler.AuthorizationValues[0]);
        Assert.Equal(freshIdToken, handler.AuthorizationValues[1]);
        Assert.Equal(AuthState.SignedIn, auth.Status.State);

        // The rotated ID token was persisted (still encrypted at rest).
        Assert.Equal(freshIdToken, store.Load()!.IdToken);
    }

    [Fact]
    public async Task A_revoked_refresh_token_surfaces_sign_in_required_without_looping()
    {
        var clock = new FakeClock();
        var store = new TokenStore(@"C:\state\tokens.bin", new FakeSecretProtector(), new FakeFileSystem());
        store.Save(new CachedTokens { RefreshToken = "revoked", IdToken = TestTokens.Jwt(clock.UtcNow.AddHours(1)) });

        var tokenClient = new FakeGoogleTokenClient();
        tokenClient.RefreshResponses.Enqueue(new TokenResponse { Error = "invalid_grant" });

        var auth = new AuthManager(
            TestTokens.Config(), tokenClient, store, new FakeBrowser(),
            new FakeLoopbackListenerFactory(() => ""), clock);

        var handler = new FakeHttpMessageHandler()
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""");

        using var http = new HttpClient(handler) { BaseAddress = new Uri("https://api.example.test/") };
        var api = new ApiClient(http, new AuthBackendCredentials(auth),
            new ApiRetryOptions { MaxRetries = 1, Backoff = _ => TimeSpan.Zero }, (_, _) => Task.CompletedTask);

        var ex = await Assert.ThrowsAsync<ApiException>(() => api.GetTranscriptAsync("t-1", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Unauthorized, ex.Kind);
        Assert.Equal(1, tokenClient.RefreshCalls);
        Assert.Single(handler.Requests);
        Assert.Equal(AuthState.SignInNeeded, auth.Status.State);
    }

    // ------------------------------------------------------------------ pause semantics

    [Fact]
    public async Task Paused_notifications_cache_the_transcript_without_copying_or_toasting()
    {
        await using var h = new Harness().Build();
        h.Paused = true;
        h.Handler
            .EnqueueJson(HttpStatusCode.OK, NegotiateJson)
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        var socket = new FakeRealtimeSocket(new[] { EventFrame("e-paused") }, holdOpenAfterFrames: true);
        var realtime = h.WithRealtime(socket);

        var results = new List<TranscriptHandlingResult>();
        h.Coordinator.Handled += r => { lock (results) { results.Add(r); } };

        realtime.Start();
        await WaitUntil(() => results.Count == 1);
        await realtime.StopAsync();

        Assert.Equal(TranscriptHandlingResult.CachedWhilePaused, results[0]);
        Assert.Null(h.Clipboard.LastText);
        Assert.Equal(0, h.Clipboard.Attempts);
        Assert.Empty(h.Notifier.Notifications);

        // But it is available for a manual copy.
        Assert.Equal(ClipboardCopyResult.Copied, await h.Coordinator.CopyLatestAsync(CancellationToken.None));
        Assert.Equal(TranscriptBody, h.Clipboard.LastText);
    }

    [Fact]
    public async Task Manual_copy_of_a_specific_history_entry_works_while_paused()
    {
        await using var h = new Harness().Build();
        h.Paused = true;
        h.History.Add(new HistoryEntry
        {
            TranscriptId = "older",
            RecordingId = "r-0",
            Preview = "older preview",
            Body = "the older body",
            CompletedAt = h.Clock.UtcNow.AddHours(-2),
            CachedAt = h.Clock.UtcNow.AddHours(-2),
            CharacterCount = 14,
        });

        Assert.Equal(ClipboardCopyResult.Copied, await h.Coordinator.CopyAsync("older", CancellationToken.None));
        Assert.Equal("the older body", h.Clipboard.LastText);

        Assert.Equal(ClipboardCopyResult.Empty, await h.Coordinator.CopyAsync("missing", CancellationToken.None));
    }

    [Fact]
    public async Task CopyLatest_reports_empty_when_nothing_is_cached()
    {
        await using var h = new Harness().Build();
        Assert.Equal(ClipboardCopyResult.Empty, await h.Coordinator.CopyLatestAsync(CancellationToken.None));
    }

    // ------------------------------------------------------------------ failure paths

    [Fact]
    public async Task A_busy_clipboard_still_caches_the_transcript_and_warns()
    {
        await using var h = new Harness().Build();
        h.Clipboard.FailuresRemaining = 99;
        h.Handler.EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        var result = await h.Coordinator.HandleAsync(
            new TranscriptCompletedEvent
            {
                Event = "transcript.completed",
                EventId = "e-busy",
                TranscriptId = "t-1",
                RecordingId = "r-1",
                Preview = "p",
            },
            CancellationToken.None);

        Assert.Equal(TranscriptHandlingResult.ClipboardFailed, result);
        Assert.NotNull(h.History.Latest());
        Assert.Equal(NotificationKind.Warning, Assert.Single(h.Notifier.Notifications).Kind);
    }

    [Fact]
    public async Task A_404_transcript_is_reported_and_nothing_is_cached()
    {
        await using var h = new Harness().Build();
        h.Handler.EnqueueProblem(HttpStatusCode.NotFound, """{"title":"Not found","status":404}""");

        var result = await h.Coordinator.HandleAsync(
            new TranscriptCompletedEvent
            {
                Event = "transcript.completed",
                EventId = "e-404",
                TranscriptId = "gone",
                RecordingId = "r",
                Preview = "p",
            },
            CancellationToken.None);

        Assert.Equal(TranscriptHandlingResult.FetchFailed, result);
        Assert.Empty(h.History.All());
        Assert.Equal(NotificationKind.Warning, Assert.Single(h.Notifier.Notifications).Kind);
    }

    [Fact]
    public async Task Expired_cached_transcripts_disappear_after_48_hours()
    {
        await using var h = new Harness().Build();
        h.Handler.EnqueueJson(HttpStatusCode.OK, TranscriptJson);

        await h.Coordinator.HandleAsync(
            new TranscriptCompletedEvent
            {
                Event = "transcript.completed",
                EventId = "e-retention",
                TranscriptId = "t-1",
                RecordingId = "r-1",
                Preview = "p",
            },
            CancellationToken.None);

        Assert.Single(h.History.All());

        h.Clock.Advance(TimeSpan.FromHours(49));

        Assert.Empty(h.History.All());
        Assert.Equal(ClipboardCopyResult.Empty, await h.Coordinator.CopyLatestAsync(CancellationToken.None));
    }

    [Fact]
    public void Notification_text_is_bounded_and_never_carries_a_body()
    {
        Assert.Equal("Ready on the clipboard.", TranscriptCoordinator.Summarize(null));
        Assert.Equal("Ready on the clipboard.", TranscriptCoordinator.Summarize("   "));
        Assert.Equal("short preview", TranscriptCoordinator.Summarize(" short preview "));

        var summary = TranscriptCoordinator.Summarize(new string('x', 400));
        Assert.Equal(120, summary.Length);
        Assert.EndsWith("…", summary);
    }
}
