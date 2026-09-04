using VoicePrompt.Core.Api;

namespace VoicePrompt.Core.Auth;

/// <summary>
/// Adapts <see cref="AuthManager"/> to the backend credential contract. Only Google
/// <b>ID tokens</b> are ever handed out — the OAuth access token is never used as the
/// backend bearer, because the backend validates <c>aud</c>/<c>email</c> from an ID token.
/// </summary>
public sealed class AuthBackendCredentials : IBackendCredentials
{
    private readonly AuthManager _auth;

    public AuthBackendCredentials(AuthManager auth) => _auth = auth;

    public Task<string?> GetIdTokenAsync(CancellationToken ct) => _auth.GetValidIdTokenAsync(ct);

    /// <summary>
    /// Exactly one refresh attempt after a 401. A transient failure also returns <c>false</c>
    /// so the API client surfaces the error instead of looping.
    /// </summary>
    public async Task<bool> TryRefreshAfterUnauthorizedAsync(CancellationToken ct)
    {
        var outcome = await _auth.ForceRefreshAsync(ct).ConfigureAwait(false);
        return outcome == RefreshOutcome.Success;
    }
}
