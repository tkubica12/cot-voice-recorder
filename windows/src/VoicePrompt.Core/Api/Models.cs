using System.Text.Json.Serialization;

namespace VoicePrompt.Core.Api;

/// <summary>Transcript body returned by <c>GET /v1/transcripts/{id}</c>.</summary>
public sealed class Transcript
{
    [JsonPropertyName("transcript_id")] public string TranscriptId { get; init; } = "";
    [JsonPropertyName("recording_id")] public string RecordingId { get; init; } = "";
    [JsonPropertyName("body")] public string Body { get; init; } = "";
    [JsonPropertyName("preview")] public string Preview { get; init; } = "";
    [JsonPropertyName("language")] public string? Language { get; init; }
    [JsonPropertyName("refine_model")] public string? RefineModel { get; init; }
    [JsonPropertyName("completed_at")] public DateTimeOffset CompletedAt { get; init; }
    [JsonPropertyName("expires_at")] public DateTimeOffset ExpiresAt { get; init; }
    [JsonPropertyName("character_count")] public int CharacterCount { get; init; }
}

/// <summary>Response from <c>POST /v1/realtime/negotiate</c>.</summary>
public sealed class NegotiateResponse
{
    [JsonPropertyName("url")] public string Url { get; init; } = "";
    [JsonPropertyName("hub")] public string Hub { get; init; } = "";
    [JsonPropertyName("group")] public string Group { get; init; } = "";
    [JsonPropertyName("expires_at")] public DateTimeOffset ExpiresAt { get; init; }
}

/// <summary>Short transcript summary used by history/list views.</summary>
public sealed class TranscriptSummary
{
    [JsonPropertyName("transcript_id")] public string TranscriptId { get; init; } = "";
    [JsonPropertyName("recording_id")] public string RecordingId { get; init; } = "";
    [JsonPropertyName("preview")] public string Preview { get; init; } = "";
    [JsonPropertyName("completed_at")] public DateTimeOffset CompletedAt { get; init; }
    [JsonPropertyName("expires_at")] public DateTimeOffset ExpiresAt { get; init; }
    [JsonPropertyName("language")] public string? Language { get; init; }
    [JsonPropertyName("refine_model")] public string? RefineModel { get; init; }
}

/// <summary>One page of <c>GET /v1/transcripts</c> results (cursor paginated).</summary>
public sealed class TranscriptListPage
{
    [JsonPropertyName("items")] public List<TranscriptSummary> Items { get; init; } = new();
    [JsonPropertyName("next_cursor")] public string? NextCursor { get; init; }
}

/// <summary>RFC 9457 Problem Details object.</summary>
public sealed class ProblemDetails
{
    [JsonPropertyName("type")] public string? Type { get; init; }
    [JsonPropertyName("title")] public string? Title { get; init; }
    [JsonPropertyName("status")] public int? Status { get; init; }
    [JsonPropertyName("detail")] public string? Detail { get; init; }
    [JsonPropertyName("instance")] public string? Instance { get; init; }
}
