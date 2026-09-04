using System.Text.Json.Serialization;

namespace VoicePrompt.Core.Auth;

/// <summary>Raw token endpoint response from Google (subset of fields the client uses).</summary>
public sealed class TokenResponse
{
    [JsonPropertyName("access_token")] public string? AccessToken { get; init; }
    [JsonPropertyName("id_token")] public string? IdToken { get; init; }
    [JsonPropertyName("refresh_token")] public string? RefreshToken { get; init; }
    [JsonPropertyName("expires_in")] public int? ExpiresIn { get; init; }
    [JsonPropertyName("token_type")] public string? TokenType { get; init; }
    [JsonPropertyName("scope")] public string? Scope { get; init; }

    // Error fields (RFC 6749 §5.2).
    [JsonPropertyName("error")] public string? Error { get; init; }
    [JsonPropertyName("error_description")] public string? ErrorDescription { get; init; }
}

/// <summary>Result classification for a refresh attempt.</summary>
public enum RefreshOutcome
{
    /// <summary>Fresh ID token obtained.</summary>
    Success,

    /// <summary>The refresh token is invalid/revoked/expired (<c>invalid_grant</c>) — sign-in required.</summary>
    RefreshRevoked,

    /// <summary>The response contained no ID token — sign-in required.</summary>
    NoIdToken,

    /// <summary>A transient error (network / 5xx) — retry later, keep current state.</summary>
    Transient,
}

/// <summary>Client for Google's OAuth 2.0 token endpoint (code exchange + refresh).</summary>
public interface IGoogleTokenClient
{
    Task<TokenResponse> ExchangeCodeAsync(
        DesktopClientConfig config, string code, string codeVerifier, string redirectUri, CancellationToken ct);

    Task<TokenResponse> RefreshAsync(
        DesktopClientConfig config, string refreshToken, CancellationToken ct);
}
