using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Auth;

/// <summary>High-level authentication state surfaced to the UI.</summary>
public enum AuthState
{
    /// <summary>No Desktop OAuth client JSON is available; sign-in is disabled safely.</summary>
    NotConfigured,

    /// <summary>No cached tokens; the user must sign in.</summary>
    SignedOut,

    /// <summary>A refresh token exists and yields fresh ID tokens.</summary>
    SignedIn,

    /// <summary>Tokens exist but refresh failed (revoked/expired/no id token); re-sign-in required.</summary>
    SignInNeeded,
}

/// <summary>Immutable snapshot of authentication status.</summary>
public sealed record AuthStatus(AuthState State, string? Email)
{
    public bool CanCallBackend => State == AuthState.SignedIn;
}

/// <summary>
/// Coordinates Google OIDC Desktop Authorization Code + PKCE sign-in, token refresh ahead of
/// expiry, and secure sign-out. Provides fresh <b>ID tokens</b> for backend bearer auth and
/// never returns an access token as the backend credential.
/// </summary>
public sealed class AuthManager
{
    private readonly DesktopClientConfig? _config;
    private readonly IGoogleTokenClient _tokenClient;
    private readonly TokenStore _store;
    private readonly ISystemBrowser _browser;
    private readonly ILoopbackAuthListenerFactory _loopbackFactory;
    private readonly IClock _clock;
    private readonly TimeSpan _refreshSkew;
    private readonly SemaphoreSlim _gate = new(1, 1);

    private CachedTokens? _tokens;
    private AuthState _state;

    public AuthManager(
        DesktopClientConfig? config,
        IGoogleTokenClient tokenClient,
        TokenStore store,
        ISystemBrowser browser,
        ILoopbackAuthListenerFactory loopbackFactory,
        IClock clock,
        TimeSpan? refreshSkew = null)
    {
        _config = config;
        _tokenClient = tokenClient;
        _store = store;
        _browser = browser;
        _loopbackFactory = loopbackFactory;
        _clock = clock;
        _refreshSkew = refreshSkew ?? TimeSpan.FromMinutes(5);

        _tokens = _store.Load();
        _state = _config is null
            ? AuthState.NotConfigured
            : (_tokens?.RefreshToken is not null ? AuthState.SignedIn : AuthState.SignedOut);
    }

    /// <summary>Fires whenever the high-level auth state or email changes.</summary>
    public event Action<AuthStatus>? StatusChanged;

    public AuthStatus Status => new(_state, _tokens?.Email);

    /// <summary>
    /// Return a fresh ID token for backend calls, refreshing ahead of expiry if needed.
    /// Returns <c>null</c> when not signed in or when a refresh determines re-sign-in is required.
    /// Never returns an access token.
    /// </summary>
    public async Task<string?> GetValidIdTokenAsync(CancellationToken ct)
    {
        if (_config is null)
        {
            return null;
        }

        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_tokens?.RefreshToken is null)
            {
                return null;
            }

            var idToken = _tokens.IdToken;
            var expiring = idToken is null
                           || SafeExpiry(idToken).IsExpiredOrExpiring(_clock.UtcNow, _refreshSkew);

            if (!expiring)
            {
                return idToken;
            }

            var outcome = await RefreshLockedAsync(ct).ConfigureAwait(false);
            return outcome == RefreshOutcome.Success ? _tokens?.IdToken : null;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>
    /// Force a single refresh (used after a backend 401). Returns the outcome so the caller can
    /// decide between retry and moving to a sign-in-needed state.
    /// </summary>
    public async Task<RefreshOutcome> ForceRefreshAsync(CancellationToken ct)
    {
        if (_config is null || _tokens?.RefreshToken is null)
        {
            return RefreshOutcome.RefreshRevoked;
        }

        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            return await RefreshLockedAsync(ct).ConfigureAwait(false);
        }
        finally
        {
            _gate.Release();
        }
    }

    // Caller holds _gate.
    private async Task<RefreshOutcome> RefreshLockedAsync(CancellationToken ct)
    {
        TokenResponse response;
        try
        {
            response = await _tokenClient.RefreshAsync(_config!, _tokens!.RefreshToken!, ct)
                .ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return RefreshOutcome.Transient;
        }

        var outcome = ClassifyRefresh(response);
        switch (outcome)
        {
            case RefreshOutcome.Success:
                _tokens!.IdToken = response.IdToken;
                _tokens.IdTokenExpiresAt = SafeExpiry(response.IdToken!).ExpiresAt;
                _tokens.Email = SafeEmail(response.IdToken!) ?? _tokens.Email;
                // Google may issue a rotated refresh token; keep the new one when present.
                if (!string.IsNullOrEmpty(response.RefreshToken))
                {
                    _tokens.RefreshToken = response.RefreshToken;
                }

                _store.Save(_tokens);
                SetState(AuthState.SignedIn);
                break;

            case RefreshOutcome.RefreshRevoked:
            case RefreshOutcome.NoIdToken:
                // Keep no stale ID token around; require a fresh interactive sign-in.
                SetState(AuthState.SignInNeeded);
                break;

            case RefreshOutcome.Transient:
                // Leave state unchanged; a later attempt may succeed.
                break;
        }

        return outcome;
    }

    /// <summary>Classify a token-endpoint response for refresh handling (pure; unit-tested).</summary>
    public static RefreshOutcome ClassifyRefresh(TokenResponse response)
    {
        if (!string.IsNullOrEmpty(response.Error))
        {
            return response.Error switch
            {
                "invalid_grant" => RefreshOutcome.RefreshRevoked,
                "server_error" or "temporarily_unavailable" => RefreshOutcome.Transient,
                _ => RefreshOutcome.RefreshRevoked,
            };
        }

        return string.IsNullOrEmpty(response.IdToken)
            ? RefreshOutcome.NoIdToken
            : RefreshOutcome.Success;
    }

    /// <summary>
    /// Run the interactive Authorization Code + PKCE flow using the system browser and a fresh
    /// ephemeral loopback listener. Persists the resulting tokens on success.
    /// </summary>
    public async Task<bool> SignInAsync(CancellationToken ct)
    {
        if (_config is null)
        {
            return false;
        }

        var pkce = PkceGenerator.Create();
        using var listener = _loopbackFactory.Create();
        var redirectUri = listener.RedirectUri;

        var authUrl = AuthorizationRequestBuilder.BuildAuthorizationUrl(_config, redirectUri, pkce);
        await _browser.OpenAsync(authUrl, ct).ConfigureAwait(false);

        var rawQuery = await listener.WaitForCallbackAsync(ct).ConfigureAwait(false);
        var code = AuthorizationRequestBuilder.ParseAndValidateCallback(rawQuery, pkce.State);

        var response = await _tokenClient
            .ExchangeCodeAsync(_config, code, pkce.CodeVerifier, redirectUri, ct)
            .ConfigureAwait(false);

        if (!string.IsNullOrEmpty(response.Error) || string.IsNullOrEmpty(response.IdToken)
            || string.IsNullOrEmpty(response.RefreshToken))
        {
            SetState(AuthState.SignInNeeded);
            return false;
        }

        _tokens = new CachedTokens
        {
            RefreshToken = response.RefreshToken,
            IdToken = response.IdToken,
            IdTokenExpiresAt = SafeExpiry(response.IdToken).ExpiresAt,
            Email = SafeEmail(response.IdToken),
        };
        _store.Save(_tokens);
        SetState(AuthState.SignedIn);
        return true;
    }

    /// <summary>Securely delete cached tokens and return to the signed-out state.</summary>
    public void SignOut()
    {
        _store.Clear();
        _tokens = null;
        SetState(_config is null ? AuthState.NotConfigured : AuthState.SignedOut);
    }

    private void SetState(AuthState state)
    {
        _state = state;
        StatusChanged?.Invoke(Status);
    }

    private static JwtToken SafeExpiry(string idToken)
    {
        try
        {
            return JwtToken.Parse(idToken);
        }
        catch (FormatException)
        {
            return new JwtToken { ExpiresAt = null };
        }
    }

    private static string? SafeEmail(string idToken)
    {
        try
        {
            return JwtToken.Parse(idToken).Email;
        }
        catch (FormatException)
        {
            return null;
        }
    }
}
