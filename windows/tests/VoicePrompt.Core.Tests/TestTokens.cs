using System.Text;
using System.Text.Json;
using VoicePrompt.Core.Auth;

namespace VoicePrompt.Core.Tests;

/// <summary>Helpers for building unsigned test JWTs and Google client JSON documents.</summary>
internal static class TestTokens
{
    public const string DesktopClientJson = """
    {
      "installed": {
        "client_id": "test-desktop-client.apps.googleusercontent.com",
        "project_id": "voiceprompt-test",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
        "client_secret": "test-not-a-real-secret",
        "redirect_uris": ["http://localhost"]
      }
    }
    """;

    public const string AndroidClientJson = """
    {
      "installed": {
        "client_id": "test-android-client.apps.googleusercontent.com",
        "project_id": "voiceprompt-test",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs"
      }
    }
    """;

    public const string WebClientJson = """
    {
      "web": {
        "client_id": "test-web-client.apps.googleusercontent.com",
        "project_id": "voiceprompt-test",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "client_secret": "test-not-a-real-secret"
      }
    }
    """;

    public static DesktopClientConfig Config() => DesktopClientConfig.Parse(DesktopClientJson);

    /// <summary>Build an unsigned JWT with the given claims (the client never verifies signatures).</summary>
    public static string Jwt(
        DateTimeOffset? expires = null,
        string? email = "owner@example.com",
        string? audience = "test-desktop-client.apps.googleusercontent.com",
        string? subject = "1234567890",
        bool omitExp = false)
    {
        var payload = new Dictionary<string, object>();
        if (!omitExp)
        {
            payload["exp"] = (expires ?? DateTimeOffset.UtcNow.AddHours(1)).ToUnixTimeSeconds();
        }

        if (email is not null)
        {
            payload["email"] = email;
            payload["email_verified"] = true;
        }

        if (audience is not null)
        {
            payload["aud"] = audience;
        }

        if (subject is not null)
        {
            payload["sub"] = subject;
        }

        payload["iss"] = "https://accounts.google.com";

        var header = Base64Url(Encoding.UTF8.GetBytes("""{"alg":"none","typ":"JWT"}"""));
        var body = Base64Url(JsonSerializer.SerializeToUtf8Bytes(payload));
        return $"{header}.{body}.";
    }

    public static string Base64Url(byte[] bytes) =>
        Convert.ToBase64String(bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_');
}
