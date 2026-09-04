using System.Text.Json;

namespace VoicePrompt.Core.Realtime;

/// <summary>The downstream frame kinds of the <c>json.webpubsub.azure.v1</c> subprotocol.</summary>
public enum WebPubSubFrameKind
{
    /// <summary>Unrecognised or unparsable frame.</summary>
    Unknown,

    /// <summary><c>{"type":"system","event":"connected|disconnected", ...}</c></summary>
    System,

    /// <summary><c>{"type":"message","from":"server|group", "dataType":..., "data":...}</c></summary>
    Message,

    /// <summary><c>{"type":"ack", ...}</c></summary>
    Ack,
}

/// <summary>
/// A parsed Azure Web PubSub server frame. The backend publishes with
/// <c>send_to_user(..., content_type="application/json")</c>, so completion events arrive as
/// <c>type=message</c>, <c>from=server</c>, <c>dataType=json</c> with the event object in
/// <c>data</c>. Group fan-out (<c>from=group</c>) is parsed identically.
/// </summary>
public sealed class WebPubSubFrame
{
    public WebPubSubFrameKind Kind { get; init; } = WebPubSubFrameKind.Unknown;

    /// <summary><c>connected</c> / <c>disconnected</c> for system frames.</summary>
    public string? SystemEvent { get; init; }

    /// <summary><c>server</c> or <c>group</c> for message frames.</summary>
    public string? From { get; init; }

    public string? Group { get; init; }

    /// <summary><c>json</c>, <c>text</c> or <c>binary</c>.</summary>
    public string? DataType { get; init; }

    public string? UserId { get; init; }

    public string? ConnectionId { get; init; }

    /// <summary>Raw JSON text of the payload, normalised across <c>json</c> and <c>text</c> data types.</summary>
    public string? PayloadJson { get; init; }

    public string? Raw { get; init; }

    public bool IsConnected => Kind == WebPubSubFrameKind.System
                               && string.Equals(SystemEvent, "connected", StringComparison.OrdinalIgnoreCase);

    public bool IsDisconnected => Kind == WebPubSubFrameKind.System
                                  && string.Equals(SystemEvent, "disconnected", StringComparison.OrdinalIgnoreCase);
}

/// <summary>Parses Web PubSub server frames without ever logging their contents.</summary>
public static class WebPubSubEnvelope
{
    /// <summary>
    /// Parse one received text frame. Never throws: malformed input yields
    /// <see cref="WebPubSubFrameKind.Unknown"/> so the receive loop stays alive.
    /// </summary>
    public static WebPubSubFrame Parse(string? text)
    {
        if (string.IsNullOrWhiteSpace(text))
        {
            return new WebPubSubFrame { Raw = text };
        }

        JsonDocument doc;
        try
        {
            doc = JsonDocument.Parse(text);
        }
        catch (JsonException)
        {
            return new WebPubSubFrame { Raw = text };
        }

        using (doc)
        {
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return new WebPubSubFrame { Raw = text };
            }

            var type = Str(root, "type");

            // A raw (no-subprotocol) connection delivers the bare payload object. Treat a
            // bare event object as a server message so both transports work.
            if (type is null)
            {
                return root.TryGetProperty("event", out _)
                    ? new WebPubSubFrame
                    {
                        Kind = WebPubSubFrameKind.Message,
                        From = "server",
                        DataType = "json",
                        PayloadJson = root.GetRawText(),
                        Raw = text,
                    }
                    : new WebPubSubFrame { Raw = text };
            }

            switch (type.ToLowerInvariant())
            {
                case "system":
                    return new WebPubSubFrame
                    {
                        Kind = WebPubSubFrameKind.System,
                        SystemEvent = Str(root, "event"),
                        UserId = Str(root, "userId"),
                        ConnectionId = Str(root, "connectionId"),
                        Raw = text,
                    };

                case "ack":
                    return new WebPubSubFrame { Kind = WebPubSubFrameKind.Ack, Raw = text };

                case "message":
                    var dataType = Str(root, "dataType") ?? "text";
                    string? payload = null;
                    if (root.TryGetProperty("data", out var data))
                    {
                        payload = data.ValueKind switch
                        {
                            // dataType=json -> data is the object itself.
                            JsonValueKind.Object or JsonValueKind.Array => data.GetRawText(),
                            // dataType=text -> data is a string that may itself contain JSON.
                            JsonValueKind.String => data.GetString(),
                            _ => null,
                        };
                    }

                    return new WebPubSubFrame
                    {
                        Kind = WebPubSubFrameKind.Message,
                        From = Str(root, "from"),
                        Group = Str(root, "group"),
                        DataType = dataType,
                        UserId = Str(root, "fromUserId"),
                        PayloadJson = payload,
                        Raw = text,
                    };

                default:
                    return new WebPubSubFrame { Raw = text };
            }
        }
    }

    /// <summary>
    /// Extract a <c>transcript.completed</c> event from a parsed frame, if present and valid.
    /// </summary>
    public static bool TryReadTranscriptCompleted(WebPubSubFrame frame, out TranscriptCompletedEvent completed)
    {
        completed = default!;
        if (frame.Kind != WebPubSubFrameKind.Message || string.IsNullOrWhiteSpace(frame.PayloadJson))
        {
            return false;
        }

        if (frame.DataType is not null
            && !frame.DataType.Equals("json", StringComparison.OrdinalIgnoreCase)
            && !frame.DataType.Equals("text", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        TranscriptCompletedEvent? parsed;
        try
        {
            parsed = JsonSerializer.Deserialize<TranscriptCompletedEvent>(frame.PayloadJson!);
        }
        catch (JsonException)
        {
            return false;
        }

        if (parsed is null || !parsed.IsTranscriptCompleted)
        {
            return false;
        }

        completed = parsed;
        return true;
    }

    private static string? Str(JsonElement obj, string name) =>
        obj.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;
}
