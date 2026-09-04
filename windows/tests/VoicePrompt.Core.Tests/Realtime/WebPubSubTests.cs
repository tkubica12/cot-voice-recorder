using VoicePrompt.Core.Realtime;
using Xunit;

namespace VoicePrompt.Core.Tests.Realtime;

public class WebPubSubEnvelopeTests
{
    private const string EventJson = """
    {
      "event": "transcript.completed",
      "event_id": "7d8e9f0a-1b2c-3d4e-5f6a-7b8c9d0e1f2a",
      "transcript_id": "1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f",
      "recording_id": "9a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
      "completed_at": "2026-09-03T13:53:10Z",
      "preview": "Připomeň mi zítra ráno zavolat doktorovi…"
    }
    """;

    [Fact]
    public void Parses_the_system_connected_frame()
    {
        var frame = WebPubSubEnvelope.Parse(
            """{"type":"system","event":"connected","userId":"owner","connectionId":"abc123"}""");

        Assert.Equal(WebPubSubFrameKind.System, frame.Kind);
        Assert.True(frame.IsConnected);
        Assert.False(frame.IsDisconnected);
        Assert.Equal("owner", frame.UserId);
        Assert.Equal("abc123", frame.ConnectionId);
    }

    [Fact]
    public void Parses_the_system_disconnected_frame()
    {
        var frame = WebPubSubEnvelope.Parse(
            """{"type":"system","event":"disconnected","message":"connection expired"}""");

        Assert.True(frame.IsDisconnected);
        Assert.False(frame.IsConnected);
    }

    [Fact]
    public void Parses_the_ack_frame()
    {
        var frame = WebPubSubEnvelope.Parse("""{"type":"ack","ackId":1,"success":true}""");

        Assert.Equal(WebPubSubFrameKind.Ack, frame.Kind);
        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }

    [Fact]
    public void Parses_a_server_message_with_a_json_payload()
    {
        // What the backend's send_to_user(content_type="application/json") produces.
        var frame = WebPubSubEnvelope.Parse(
            $$"""{"type":"message","from":"server","dataType":"json","data":{{EventJson}}}""");

        Assert.Equal(WebPubSubFrameKind.Message, frame.Kind);
        Assert.Equal("server", frame.From);
        Assert.Equal("json", frame.DataType);

        Assert.True(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out var evt));
        Assert.Equal("transcript.completed", evt.Event);
        Assert.Equal("7d8e9f0a-1b2c-3d4e-5f6a-7b8c9d0e1f2a", evt.EventId);
        Assert.Equal("1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f", evt.TranscriptId);
        Assert.Equal("9a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d", evt.RecordingId);
        Assert.StartsWith("Připomeň", evt.Preview);
        Assert.Equal(
            new DateTimeOffset(2026, 9, 3, 13, 53, 10, TimeSpan.Zero),
            evt.CompletedAt.ToUniversalTime());
    }

    [Fact]
    public void Parses_a_group_message_with_a_json_payload()
    {
        var frame = WebPubSubEnvelope.Parse(
            $$"""{"type":"message","from":"group","group":"user-allowlisted","fromUserId":"owner","dataType":"json","data":{{EventJson}}}""");

        Assert.Equal("group", frame.From);
        Assert.Equal("user-allowlisted", frame.Group);
        Assert.Equal("owner", frame.UserId);
        Assert.True(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }

    [Fact]
    public void Parses_a_text_data_type_carrying_json_as_a_string()
    {
        var escaped = System.Text.Json.JsonSerializer.Serialize(EventJson);
        var frame = WebPubSubEnvelope.Parse(
            $$"""{"type":"message","from":"server","dataType":"text","data":{{escaped}}}""");

        Assert.Equal("text", frame.DataType);
        Assert.True(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out var evt));
        Assert.Equal("transcript.completed", evt.Event);
    }

    [Fact]
    public void Accepts_a_bare_event_object_from_a_raw_connection()
    {
        var frame = WebPubSubEnvelope.Parse(EventJson);

        Assert.Equal(WebPubSubFrameKind.Message, frame.Kind);
        Assert.True(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out var evt));
        Assert.Equal("transcript.completed", evt.Event);
    }

    [Fact]
    public void Rejects_a_binary_data_type()
    {
        var frame = WebPubSubEnvelope.Parse(
            """{"type":"message","from":"server","dataType":"binary","data":"AAEC"}""");

        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }

    [Fact]
    public void Rejects_a_different_event_type()
    {
        var frame = WebPubSubEnvelope.Parse(
            """{"type":"message","from":"server","dataType":"json","data":{"event":"something.else","event_id":"1","transcript_id":"2"}}""");

        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }

    [Fact]
    public void Rejects_an_event_missing_required_ids()
    {
        var missingEventId = WebPubSubEnvelope.Parse(
            """{"type":"message","from":"server","dataType":"json","data":{"event":"transcript.completed","transcript_id":"t"}}""");
        var missingTranscriptId = WebPubSubEnvelope.Parse(
            """{"type":"message","from":"server","dataType":"json","data":{"event":"transcript.completed","event_id":"e"}}""");

        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(missingEventId, out _));
        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(missingTranscriptId, out _));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("not json at all")]
    [InlineData("[1,2,3]")]
    [InlineData("\"a string\"")]
    [InlineData("""{"type":"unknown-kind"}""")]
    [InlineData("""{"type":"message","from":"server","dataType":"json"}""")]
    public void Never_throws_on_malformed_frames(string? text)
    {
        var frame = WebPubSubEnvelope.Parse(text);
        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }

    [Fact]
    public void Payload_that_is_not_valid_json_is_ignored()
    {
        var frame = WebPubSubEnvelope.Parse(
            """{"type":"message","from":"server","dataType":"text","data":"hello world"}""");

        Assert.False(WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out _));
    }
}

public class EventDeduplicatorTests
{
    [Fact]
    public void First_sighting_is_processed_and_repeats_are_suppressed()
    {
        var dedupe = new EventDeduplicator();

        Assert.True(dedupe.TryMarkSeen("e1"));
        Assert.False(dedupe.TryMarkSeen("e1"));
        Assert.False(dedupe.TryMarkSeen("e1"));
        Assert.True(dedupe.TryMarkSeen("e2"));
        Assert.True(dedupe.HasSeen("e1"));
        Assert.False(dedupe.HasSeen("missing"));
    }

    [Fact]
    public void Empty_ids_are_always_processed()
    {
        var dedupe = new EventDeduplicator();

        Assert.True(dedupe.TryMarkSeen(null));
        Assert.True(dedupe.TryMarkSeen(""));
        Assert.Equal(0, dedupe.Count);
    }

    [Fact]
    public void The_window_is_bounded_and_evicts_oldest_first()
    {
        var dedupe = new EventDeduplicator(capacity: 3);

        Assert.True(dedupe.TryMarkSeen("a"));
        Assert.True(dedupe.TryMarkSeen("b"));
        Assert.True(dedupe.TryMarkSeen("c"));
        Assert.Equal(3, dedupe.Count);

        Assert.True(dedupe.TryMarkSeen("d"));   // evicts "a"
        Assert.Equal(3, dedupe.Count);
        Assert.False(dedupe.HasSeen("a"));
        Assert.True(dedupe.HasSeen("b"));
        Assert.False(dedupe.TryMarkSeen("c"));
    }

    [Fact]
    public void Clear_forgets_everything()
    {
        var dedupe = new EventDeduplicator();
        dedupe.TryMarkSeen("x");

        dedupe.Clear();

        Assert.Equal(0, dedupe.Count);
        Assert.True(dedupe.TryMarkSeen("x"));
    }

    [Fact]
    public void Rejects_a_non_positive_capacity() =>
        Assert.Throws<ArgumentOutOfRangeException>(() => new EventDeduplicator(0));

    [Fact]
    public async Task Is_safe_under_concurrent_marking()
    {
        var dedupe = new EventDeduplicator(capacity: 1024);
        var wins = 0;

        await Task.WhenAll(Enumerable.Range(0, 32).Select(_ => Task.Run(() =>
        {
            if (dedupe.TryMarkSeen("same-event"))
            {
                Interlocked.Increment(ref wins);
            }
        })));

        Assert.Equal(1, wins);
    }
}

public class ReconnectPolicyTests
{
    [Fact]
    public void Ceiling_grows_exponentially_from_the_initial_delay()
    {
        var policy = new ReconnectPolicy(TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(60), 2.0, () => 1.0);

        Assert.Equal(TimeSpan.FromSeconds(1), policy.Ceiling(1));
        Assert.Equal(TimeSpan.FromSeconds(2), policy.Ceiling(2));
        Assert.Equal(TimeSpan.FromSeconds(4), policy.Ceiling(3));
        Assert.Equal(TimeSpan.FromSeconds(8), policy.Ceiling(4));
    }

    [Fact]
    public void Ceiling_is_capped_and_never_overflows()
    {
        var policy = new ReconnectPolicy(TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(60));

        Assert.Equal(TimeSpan.FromSeconds(60), policy.Ceiling(50));
        Assert.Equal(TimeSpan.FromSeconds(60), policy.Ceiling(10_000));
        Assert.Equal(TimeSpan.FromSeconds(1), policy.Ceiling(0));
        Assert.Equal(TimeSpan.FromSeconds(1), policy.Ceiling(-5));
    }

    [Fact]
    public void Delay_is_jittered_within_half_of_the_ceiling()
    {
        var policy = new ReconnectPolicy(TimeSpan.FromSeconds(4), TimeSpan.FromSeconds(60), 2.0, () => 0.0);
        Assert.Equal(TimeSpan.FromSeconds(2), policy.NextDelay(1));

        var upper = new ReconnectPolicy(TimeSpan.FromSeconds(4), TimeSpan.FromSeconds(60), 2.0, () => 1.0);
        Assert.Equal(TimeSpan.FromSeconds(4), upper.NextDelay(1));
    }

    [Fact]
    public void Random_delays_stay_inside_the_bounds_and_actually_vary()
    {
        var policy = new ReconnectPolicy(TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(30));
        var samples = Enumerable.Range(1, 200).Select(i => policy.NextDelay((i % 8) + 1)).ToList();

        Assert.All(samples, d =>
        {
            Assert.True(d > TimeSpan.Zero);
            Assert.True(d <= TimeSpan.FromSeconds(30));
        });

        Assert.True(samples.Distinct().Count() > 5, "expected jitter to produce varied delays");
    }

    [Fact]
    public void An_invalid_factor_falls_back_to_doubling()
    {
        var policy = new ReconnectPolicy(TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(60), factor: 0.5);
        Assert.Equal(TimeSpan.FromSeconds(2), policy.Ceiling(2));
    }
}
