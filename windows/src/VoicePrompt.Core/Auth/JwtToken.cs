using System.Text;
using System.Text.Json;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// Minimal read-only parsing of a JWT (ID token) payload. The client does not verify the
/// signature — that is the backend's job — it only needs <c>exp</c> for refresh scheduling
/// and <c>email</c>/<c>aud</c> for display and diagnostics.
/// </summary>
public sealed class JwtToken
{
    public DateTimeOffset? ExpiresAt { get; init; }
    public string? Email { get; init; }
    public string? Audience { get; init; }
    public string? Subject { get; init; }

    /// <summary>Parse the payload segment of a compact JWT. Throws <see cref="FormatException"/> on malformed input.</summary>
    public static JwtToken Parse(string jwt)
    {
        if (string.IsNullOrWhiteSpace(jwt))
        {
            throw new FormatException("Empty JWT.");
        }

        var parts = jwt.Split('.');
        if (parts.Length < 2)
        {
            throw new FormatException("JWT must have at least header and payload segments.");
        }

        var payloadJson = Encoding.UTF8.GetString(Base64UrlDecode(parts[1]));
        using var doc = JsonDocument.Parse(payloadJson);
        var root = doc.RootElement;

        DateTimeOffset? exp = null;
        if (root.TryGetProperty("exp", out var expEl) && expEl.ValueKind == JsonValueKind.Number)
        {
            exp = DateTimeOffset.FromUnixTimeSeconds(expEl.GetInt64());
        }

        string? email = root.TryGetProperty("email", out var e) && e.ValueKind == JsonValueKind.String
            ? e.GetString()
            : null;

        string? aud = null;
        if (root.TryGetProperty("aud", out var audEl))
        {
            aud = audEl.ValueKind == JsonValueKind.Array
                ? (audEl.GetArrayLength() > 0 ? audEl[0].GetString() : null)
                : audEl.GetString();
        }

        string? sub = root.TryGetProperty("sub", out var s) && s.ValueKind == JsonValueKind.String
            ? s.GetString()
            : null;

        return new JwtToken { ExpiresAt = exp, Email = email, Audience = aud, Subject = sub };
    }

    /// <summary>True when the token expires within <paramref name="skew"/> of <paramref name="now"/>.</summary>
    public bool IsExpiredOrExpiring(DateTimeOffset now, TimeSpan skew)
    {
        if (ExpiresAt is null)
        {
            return true;
        }

        return ExpiresAt.Value - skew <= now;
    }

    internal static byte[] Base64UrlDecode(string input)
    {
        var s = input.Replace('-', '+').Replace('_', '/');
        switch (s.Length % 4)
        {
            case 2: s += "=="; break;
            case 3: s += "="; break;
            case 1: throw new FormatException("Invalid base64url length.");
        }

        return Convert.FromBase64String(s);
    }
}
