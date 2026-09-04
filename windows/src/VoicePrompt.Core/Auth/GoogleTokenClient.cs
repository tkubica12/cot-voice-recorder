using System.Net.Http.Json;
using System.Text.Json;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// Talks to Google's OAuth token endpoint using <c>application/x-www-form-urlencoded</c>
/// requests. Handles both authorization-code exchange and refresh. Never logs token values.
/// </summary>
public sealed class GoogleTokenClient : IGoogleTokenClient
{
    private readonly HttpClient _http;

    public GoogleTokenClient(HttpClient http) => _http = http;

    public Task<TokenResponse> ExchangeCodeAsync(
        DesktopClientConfig config, string code, string codeVerifier, string redirectUri, CancellationToken ct)
    {
        var form = new Dictionary<string, string>
        {
            ["client_id"] = config.ClientId,
            ["client_secret"] = config.ClientSecret,
            ["code"] = code,
            ["code_verifier"] = codeVerifier,
            ["grant_type"] = "authorization_code",
            ["redirect_uri"] = redirectUri,
        };
        return PostAsync(config.TokenUri, form, ct);
    }

    public Task<TokenResponse> RefreshAsync(
        DesktopClientConfig config, string refreshToken, CancellationToken ct)
    {
        var form = new Dictionary<string, string>
        {
            ["client_id"] = config.ClientId,
            ["client_secret"] = config.ClientSecret,
            ["refresh_token"] = refreshToken,
            ["grant_type"] = "refresh_token",
        };
        return PostAsync(config.TokenUri, form, ct);
    }

    private async Task<TokenResponse> PostAsync(
        string url, Dictionary<string, string> form, CancellationToken ct)
    {
        using var content = new FormUrlEncodedContent(form);
        using var response = await _http.PostAsync(url, content, ct).ConfigureAwait(false);
        var body = await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);

        TokenResponse? parsed = null;
        if (!string.IsNullOrWhiteSpace(body))
        {
            try
            {
                parsed = JsonSerializer.Deserialize<TokenResponse>(body);
            }
            catch (JsonException)
            {
                // Fall through to the status-based handling below.
            }
        }

        if (response.IsSuccessStatusCode)
        {
            return parsed ?? new TokenResponse();
        }

        // Non-2xx: return the parsed error (so RefreshOutcome can classify invalid_grant),
        // or synthesize one from the status code for retry classification.
        if (parsed is not null && parsed.Error is not null)
        {
            return parsed;
        }

        return new TokenResponse
        {
            Error = ((int)response.StatusCode) >= 500 ? "server_error" : "invalid_request",
            ErrorDescription = $"HTTP {(int)response.StatusCode}",
        };
    }
}
