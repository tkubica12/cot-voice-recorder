using System.Runtime.Versioning;
using System.Security.Cryptography;

namespace VoicePrompt.Core.Infrastructure;

/// <summary>
/// Encrypts/decrypts small blobs at rest. The production implementation uses Windows
/// DPAPI (CurrentUser scope); tests substitute a reversible fake so the token-store
/// branches can be exercised on any platform.
/// </summary>
public interface ISecretProtector
{
    byte[] Protect(byte[] plaintext);
    byte[] Unprotect(byte[] ciphertext);
}

/// <summary>Windows DPAPI protector, scoped to the current user under LocalAppData.</summary>
[SupportedOSPlatform("windows")]
public sealed class DpapiSecretProtector : ISecretProtector
{
    // A static entropy value adds a small amount of app-specific binding on top of the
    // user scope. It is not a secret; DPAPI's security comes from the user's Windows creds.
    private static readonly byte[] Entropy =
        System.Text.Encoding.UTF8.GetBytes("VoicePrompt.DPAPI.v1");

    public byte[] Protect(byte[] plaintext) =>
        ProtectedData.Protect(plaintext, Entropy, DataProtectionScope.CurrentUser);

    public byte[] Unprotect(byte[] ciphertext) =>
        ProtectedData.Unprotect(ciphertext, Entropy, DataProtectionScope.CurrentUser);
}
