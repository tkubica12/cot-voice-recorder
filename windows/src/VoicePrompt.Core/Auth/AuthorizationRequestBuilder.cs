using System.Collections.Specialized;
using System.Web;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// Builds a Google OAuth 2.0 Authorization Code + PKCE request URL and validates the
/// redirect callback. Scopes are limited to <c>openid email profile</c>; offline access is
/// requested so a refresh token is issued.
/// </summary>
public static class AuthorizationRequestBuilder
{
    /// <summary>
    /// Build the browser authorization URL for the given client, loopback redirect URI, and
    /// PKCE pair.
    /// </summary>
    public static string BuildAuthorizationUrl(
        DesktopClientConfig config,
        string redirectUri,
        PkcePair pkce,
        IEnumerable<string>? scopes = null,
        string? loginHint = null)
    {
        var query = HttpUtility.ParseQueryString(string.Empty);
        query["client_id"] = config.ClientId;
        query["response_type"] = "code";
        query["redirect_uri"] = redirectUri;
        query["scope"] = string.Join(' ', scopes ?? DesktopClientConfig.Scopes);
        query["code_challenge"] = pkce.CodeChallenge;
        query["code_challenge_method"] = pkce.CodeChallengeMethod;
        query["state"] = pkce.State;
        // Request a refresh token and force the consent prompt so offline access is granted.
        query["access_type"] = "offline";
        query["prompt"] = "consent";
        if (!string.IsNullOrEmpty(loginHint))
        {
            query["login_hint"] = loginHint;
        }

        var separator = config.AuthUri.Contains('?') ? "&" : "?";
        return config.AuthUri + separator + query;
    }

    /// <summary>
    /// Parse an OAuth redirect callback query string and validate the <c>state</c> against the
    /// expected value. Returns the authorization code, or throws on state mismatch / error.
    /// </summary>
    public static string ParseAndValidateCallback(string rawQuery, string expectedState)
    {
        NameValueCollection parsed = HttpUtility.ParseQueryString(
            rawQuery.StartsWith('?') ? rawQuery[1..] : rawQuery);

        var error = parsed["error"];
        if (!string.IsNullOrEmpty(error))
        {
            throw new AuthCallbackException($"Authorization failed: {error}.");
        }

        var state = parsed["state"];
        if (string.IsNullOrEmpty(state) || !FixedTimeEquals(state, expectedState))
        {
            throw new AuthCallbackException("OAuth state mismatch (possible CSRF); rejecting callback.");
        }

        var code = parsed["code"];
        if (string.IsNullOrEmpty(code))
        {
            throw new AuthCallbackException("Authorization callback did not include a code.");
        }

        return code;
    }

    private static bool FixedTimeEquals(string a, string b)
    {
        var ba = System.Text.Encoding.UTF8.GetBytes(a);
        var bb = System.Text.Encoding.UTF8.GetBytes(b);
        return System.Security.Cryptography.CryptographicOperations.FixedTimeEquals(ba, bb);
    }
}

/// <summary>Raised when the OAuth redirect callback is invalid (error, missing code, or bad state).</summary>
public sealed class AuthCallbackException : Exception
{
    public AuthCallbackException(string message) : base(message) { }
}
