using System.Text.Json.Serialization;

namespace VoicePrompt.Core.Realtime;

/// <summary>
/// The <c>transcript.completed</c> notification delivered over Web PubSub. Carries IDs and a
/// short preview only — never the full transcript body.
/// </summary>
public sealed class TranscriptCompletedEvent
{
    [JsonPropertyName("event")] public string Event { get; init; } = "";
    [JsonPropertyName("event_id")] public string EventId { get; init; } = "";
    [JsonPropertyName("transcript_id")] public string TranscriptId { get; init; } = "";
    [JsonPropertyName("recording_id")] public string RecordingId { get; init; } = "";
    [JsonPropertyName("completed_at")] public DateTimeOffset CompletedAt { get; init; }
    [JsonPropertyName("preview")] public string Preview { get; init; } = "";

    public const string EventName = "transcript.completed";

    public bool IsTranscriptCompleted =>
        string.Equals(Event, EventName, StringComparison.Ordinal)
        && !string.IsNullOrEmpty(EventId)
        && !string.IsNullOrEmpty(TranscriptId);
}
