using System.Text;
using System.Text.Json;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Auth;

/// <summary>The set of tokens cached at rest (encrypted).</summary>
public sealed class CachedTokens
{
    public string? RefreshToken { get; set; }
    public string? IdToken { get; set; }
    public DateTimeOffset? IdTokenExpiresAt { get; set; }
    public string? Email { get; set; }
}

/// <summary>
/// Persists OAuth tokens encrypted at rest. The serialized JSON is encrypted with an
/// <see cref="ISecretProtector"/> (Windows DPAPI in production) before it is atomically
/// written under LocalAppData. Corrupt or undecryptable files are treated as "no tokens".
/// </summary>
public sealed class TokenStore
{
    private readonly string _path;
    private readonly ISecretProtector _protector;
    private readonly IFileSystem _fs;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull,
    };

    public TokenStore(string path, ISecretProtector protector, IFileSystem fs)
    {
        _path = path;
        _protector = protector;
        _fs = fs;
    }

    /// <summary>Load and decrypt cached tokens, or <c>null</c> if none / unreadable.</summary>
    public CachedTokens? Load()
    {
        if (!_fs.FileExists(_path))
        {
            return null;
        }

        try
        {
            var cipher = _fs.ReadAllBytes(_path);
            if (cipher.Length == 0)
            {
                return null;
            }

            var plain = _protector.Unprotect(cipher);
            var json = Encoding.UTF8.GetString(plain);
            return JsonSerializer.Deserialize<CachedTokens>(json, JsonOptions);
        }
        catch
        {
            // Undecryptable (e.g. copied to another user/machine) or corrupt: treat as none.
            return null;
        }
    }

    /// <summary>Encrypt and atomically persist the tokens.</summary>
    public void Save(CachedTokens tokens)
    {
        var json = JsonSerializer.SerializeToUtf8Bytes(tokens, JsonOptions);
        var cipher = _protector.Protect(json);
        _fs.AtomicWrite(_path, cipher);
    }

    /// <summary>Securely delete cached tokens (sign-out).</summary>
    public void Clear() => _fs.Delete(_path);
}
