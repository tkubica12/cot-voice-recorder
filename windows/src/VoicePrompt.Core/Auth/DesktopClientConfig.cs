using System.Text.Json;
using System.Text.Json.Serialization;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// The Google "Desktop app" OAuth client configuration, parsed from the downloaded
/// <c>installed</c> client JSON. The <see cref="ClientSecret"/> of a public desktop client
/// is not truly confidential, but it is still treated as sensitive operational config:
/// it is loaded only from a git-ignored file and never logged.
/// </summary>
public sealed class DesktopClientConfig
{
    public required string ClientId { get; init; }
    public required string ClientSecret { get; init; }
    public required string AuthUri { get; init; }
    public required string TokenUri { get; init; }

    /// <summary>OIDC scopes. Deliberately limited to non-sensitive basic scopes.</summary>
    public static readonly IReadOnlyList<string> Scopes = new[] { "openid", "email", "profile" };

    /// <summary>
    /// Parse the Google "installed" client JSON. Accepts both the <c>installed</c> and
    /// (defensively) <c>web</c> roots so a mis-download is still readable; the desktop flow
    /// requires the client secret to be present.
    /// </summary>
    public static DesktopClientConfig Parse(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;
        JsonElement node;
        if (root.TryGetProperty("installed", out var installed))
        {
            node = installed;
        }
        else if (root.TryGetProperty("web", out var web))
        {
            node = web;
        }
        else
        {
            throw new FormatException("OAuth client JSON has neither 'installed' nor 'web' root.");
        }

        string GetString(string name) =>
            node.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
                ? v.GetString()!
                : throw new FormatException($"OAuth client JSON is missing '{name}'.");

        string GetOptional(string name, string fallback) =>
            node.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
                ? v.GetString()!
                : fallback;

        return new DesktopClientConfig
        {
            ClientId = GetString("client_id"),
            ClientSecret = GetString("client_secret"),
            AuthUri = GetOptional("auth_uri", "https://accounts.google.com/o/oauth2/auth"),
            TokenUri = GetOptional("token_uri", "https://oauth2.googleapis.com/token"),
        };
    }

    /// <summary>True when the JSON looks like a usable desktop client (root + client secret).</summary>
    public static bool LooksLikeDesktopClient(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (!root.TryGetProperty("installed", out var node))
            {
                return false;
            }

            return node.TryGetProperty("client_secret", out var s)
                   && s.ValueKind == JsonValueKind.String
                   && !string.IsNullOrEmpty(s.GetString());
        }
        catch (JsonException)
        {
            return false;
        }
    }
}
