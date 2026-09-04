using VoicePrompt.Core.Api;
using VoicePrompt.Core.Realtime;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Realtime;

public class RealtimeClientTests
{
    private const string EventFrame = """
    {"type":"message","from":"server","dataType":"json","data":{"event":"transcript.completed","event_id":"e1","transcript_id":"t1","recording_id":"r1","completed_at":"2026-09-03T13:53:10Z","preview":"hello"}}
    """;

    private static NegotiateResponse Access(DateTimeOffset expiresAt, string url = "wss://wps.example.test/client/hubs/transcripts?access_token=abc") =>
        new() { Url = url, Hub = "transcripts", Group = "user", ExpiresAt = expiresAt };

    private static RealtimeClient Build(
        FakeNegotiator negotiator,
        FakeRealtimeSocketFactory sockets,
        FakeClock clock,
        out List<RealtimeState> states,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        var client = new RealtimeClient(
            negotiator,
            sockets,
            clock,
            log: null,
            policy: new ReconnectPolicy(TimeSpan.FromMilliseconds(1), TimeSpan.FromMilliseconds(5)),
            renewSkew: TimeSpan.FromMinutes(2),
            signInRetryDelay: TimeSpan.FromMilliseconds(5),
            delay: delay ?? ((_, _) => Task.CompletedTask));

        var observed = new List<RealtimeState>();
        client.StateChanged += s => { lock (observed) { observed.Add(s); } };
        states = observed;
        return client;
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

    [Fact]
    public async Task Negotiates_connects_and_delivers_a_transcript_completed_event()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator().Enqueue(Access(clock.UtcNow.AddHours(1)));
        var socket = new FakeRealtimeSocket(new[] { """{"type":"system","event":"connected","userId":"owner"}""", EventFrame }, holdOpenAfterFrames: true);
        var sockets = new FakeRealtimeSocketFactory(socket);

        await using var client = Build(negotiator, sockets, clock, out var states);

        var received = new List<TranscriptCompletedEvent>();
        client.TranscriptCompleted += (e, _) =>
        {
            lock (received) { received.Add(e); }

            return Task.CompletedTask;
        };

        client.Start();
        await WaitUntil(() => received.Count == 1);
        await client.StopAsync();

        Assert.Equal("e1", received[0].EventId);
        Assert.Equal("t1", received[0].TranscriptId);
        Assert.Equal(1, negotiator.Calls);
        Assert.Equal(new Uri("wss://wps.example.test/client/hubs/transcripts?access_token=abc"), socket.ConnectedTo);
        Assert.Contains(RealtimeState.Connected, states);
        Assert.True(socket.Closed);
        Assert.True(socket.Disposed);
    }

    [Fact]
    public async Task Reconnects_with_a_fresh_negotiate_after_the_peer_closes()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .Enqueue(Access(clock.UtcNow.AddHours(1)))
            .Enqueue(Access(clock.UtcNow.AddHours(1), "wss://wps.example.test/second?access_token=xyz"));

        var first = new FakeRealtimeSocket(Array.Empty<string>());                     // closes immediately
        var second = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);
        var sockets = new FakeRealtimeSocketFactory(first, second);

        await using var client = Build(negotiator, sockets, clock, out _);

        var received = 0;
        client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

        client.Start();
        await WaitUntil(() => Volatile.Read(ref received) == 1);
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
        Assert.Equal(new Uri("wss://wps.example.test/second?access_token=xyz"), second.ConnectedTo);
    }

    [Fact]
    public async Task Renews_the_access_url_before_it_expires()
    {
        var clock = new FakeClock();

        // First URL expires inside the renewal skew, so the client must renegotiate promptly.
        var negotiator = new FakeNegotiator()
            .Enqueue(Access(clock.UtcNow.AddMinutes(2).AddMilliseconds(50)))
            .Enqueue(Access(clock.UtcNow.AddHours(1), "wss://wps.example.test/renewed?access_token=new"));

        var first = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        var second = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);
        var sockets = new FakeRealtimeSocketFactory(first, second);

        await using var client = Build(negotiator, sockets, clock, out _);

        var received = 0;
        client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

        client.Start();
        await WaitUntil(() => Volatile.Read(ref received) == 1);
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
        Assert.True(first.Closed);
        Assert.Equal(new Uri("wss://wps.example.test/renewed?access_token=new"), second.ConnectedTo);
    }

    [Fact]
    public async Task Backs_off_between_failed_attempts_and_eventually_connects()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .EnqueueThrow(new HttpRequestException("dns failure"))
            .EnqueueThrow(new HttpRequestException("dns failure"))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var socket = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);
        var delays = new List<TimeSpan>();

        var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out var states,
            delay: (d, _) => { lock (delays) { delays.Add(d); } return Task.CompletedTask; });

        await using (client)
        {
            var received = 0;
            client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

            client.Start();
            await WaitUntil(() => Volatile.Read(ref received) == 1);
            await client.StopAsync();
        }

        Assert.Equal(3, negotiator.Calls);
        Assert.Equal(2, delays.Count);
        Assert.All(delays, d => Assert.True(d > TimeSpan.Zero && d <= TimeSpan.FromMilliseconds(5)));
        Assert.Contains(RealtimeState.Reconnecting, states);
    }

    [Fact]
    public async Task A_401_negotiate_moves_to_sign_in_required()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .EnqueueThrow(new ApiException(ApiErrorKind.Unauthorized, 401, null, "unauthorized"))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var socket = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out var states);

        client.Start();
        await WaitUntil(() => states.Contains(RealtimeState.SignInRequired));
        await client.StopAsync();

        Assert.Contains(RealtimeState.SignInRequired, states);
    }

    [Fact]
    public async Task A_403_negotiate_moves_to_sign_in_required()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .EnqueueThrow(new ApiException(ApiErrorKind.Forbidden, 403, null, "forbidden"))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var socket = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out var states);

        client.Start();
        await WaitUntil(() => states.Contains(RealtimeState.SignInRequired));
        await client.StopAsync();
    }

    [Fact]
    public async Task Reconnect_interrupts_the_sign_in_retry_wait_after_interactive_auth()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .EnqueueThrow(new ApiException(ApiErrorKind.Unauthorized, 401, null, "unauthorized"))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));
        var socket = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        var retryStarted = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        async Task BlockingDelay(TimeSpan _, CancellationToken ct)
        {
            retryStarted.TrySetResult(true);
            await Task.Delay(Timeout.InfiniteTimeSpan, ct);
        }

        await using var client = Build(
            negotiator,
            new FakeRealtimeSocketFactory(socket),
            clock,
            out var states,
            BlockingDelay);

        client.Start();
        await retryStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));

        client.Reconnect();

        await WaitUntil(() => states.Contains(RealtimeState.Connected));
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
        Assert.Equal(RealtimeState.Stopped, client.State);
    }

    [Fact]
    public async Task Suspend_drops_the_connection_and_Resume_reconnects()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .Enqueue(Access(clock.UtcNow.AddHours(1)))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var first = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        var second = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);
        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(first, second), clock, out var states);

        var received = 0;
        client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

        client.Start();
        await WaitUntil(() => states.Contains(RealtimeState.Connected));

        client.Suspend();
        await WaitUntil(() => first.Closed && client.State == RealtimeState.Suspended);
        Assert.True(first.Closed);
        Assert.Equal(1, negotiator.Calls);

        client.Resume();
        await WaitUntil(() => Volatile.Read(ref received) == 1);
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
    }

    [Fact]
    public async Task A_failing_event_handler_does_not_kill_the_connection()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator().Enqueue(Access(clock.UtcNow.AddHours(1)));
        var socket = new FakeRealtimeSocket(new[] { EventFrame, EventFrame }, holdOpenAfterFrames: true);

        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out _);

        var seen = 0;
        client.TranscriptCompleted += (_, _) =>
        {
            Interlocked.Increment(ref seen);
            throw new InvalidOperationException("handler blew up");
        };

        client.Start();
        await WaitUntil(() => Volatile.Read(ref seen) == 2);
        await client.StopAsync();

        // Only one negotiate: the connection survived both handler failures.
        Assert.Equal(1, negotiator.Calls);
    }

    [Fact]
    public async Task A_service_disconnected_frame_triggers_a_reconnect()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .Enqueue(Access(clock.UtcNow.AddHours(1)))
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var first = new FakeRealtimeSocket(
            new[] { """{"type":"system","event":"disconnected","message":"expired"}""" },
            holdOpenAfterFrames: true);
        var second = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);

        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(first, second), clock, out _);

        var received = 0;
        client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

        client.Start();
        await WaitUntil(() => Volatile.Read(ref received) == 1);
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
    }

    [Fact]
    public async Task An_empty_negotiate_url_is_treated_as_a_retryable_failure()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator()
            .Enqueue(new NegotiateResponse { Url = "", Hub = "h", Group = "g", ExpiresAt = clock.UtcNow.AddHours(1) })
            .Enqueue(Access(clock.UtcNow.AddHours(1)));

        var socket = new FakeRealtimeSocket(new[] { EventFrame }, holdOpenAfterFrames: true);
        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out _);

        var received = 0;
        client.TranscriptCompleted += (_, _) => { Interlocked.Increment(ref received); return Task.CompletedTask; };

        client.Start();
        await WaitUntil(() => Volatile.Read(ref received) == 1);
        await client.StopAsync();

        Assert.Equal(2, negotiator.Calls);
    }

    [Fact]
    public async Task Start_is_idempotent_and_Stop_returns_to_stopped()
    {
        var clock = new FakeClock();
        var negotiator = new FakeNegotiator().Enqueue(Access(clock.UtcNow.AddHours(1)));
        var socket = new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        await using var client = Build(negotiator, new FakeRealtimeSocketFactory(socket), clock, out var states);

        client.Start();
        client.Start();
        await WaitUntil(() => states.Contains(RealtimeState.Connected));

        await client.StopAsync();

        Assert.Equal(RealtimeState.Stopped, client.State);
        Assert.Equal(1, negotiator.Calls);
        Assert.False(client.IsRunning);
    }

    [Fact]
    public async Task Stopping_a_never_started_client_is_safe()
    {
        var client = Build(new FakeNegotiator(), new FakeRealtimeSocketFactory(), new FakeClock(), out _);
        await client.StopAsync();
        Assert.Equal(RealtimeState.Stopped, client.State);
    }
}
