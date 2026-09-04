using System.Security.Cryptography;
using System.Text;

namespace VoicePrompt.Core.Auth;

/// <summary>A PKCE verifier/challenge pair plus a CSRF <c>state</c> value.</summary>
public sealed record PkcePair(string CodeVerifier, string CodeChallenge, string State)
{
    public string CodeChallengeMethod => "S256";
}

/// <summary>
/// Generates RFC 7636 PKCE values and an opaque <c>state</c>. Uses a cryptographic RNG and
/// base64url (no padding) encoding as required by Google's OAuth endpoints.
/// </summary>
public static class PkceGenerator
{
    /// <summary>Create a fresh verifier (43-128 chars), its S256 challenge, and a state value.</summary>
    public static PkcePair Create(int verifierBytes = 32, int stateBytes = 16)
    {
        if (verifierBytes < 32 || verifierBytes > 96)
        {
            throw new ArgumentOutOfRangeException(nameof(verifierBytes),
                "verifierBytes must yield a 43-128 char verifier (32-96 bytes).");
        }

        var verifier = Base64Url(RandomNumberGenerator.GetBytes(verifierBytes));
        var challenge = Challenge(verifier);
        var state = Base64Url(RandomNumberGenerator.GetBytes(stateBytes));
        return new PkcePair(verifier, challenge, state);
    }

    /// <summary>Compute the S256 code challenge for a given verifier.</summary>
    public static string Challenge(string codeVerifier)
    {
        var hash = SHA256.HashData(Encoding.ASCII.GetBytes(codeVerifier));
        return Base64Url(hash);
    }

    public static string Base64Url(byte[] bytes) =>
        Convert.ToBase64String(bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_');
}
