using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Auth;

public class AuthManagerTests
{
    private const string Path = @"C:\state\tokens.bin";

    private sealed class Harness
    {
        public FakeFileSystem Fs { get; } = new();
        public FakeSecretProtector Protector { get; } = new();
        public FakeGoogleTokenClient TokenClient { get; } = new();
        public FakeBrowser Browser { get; } = new();
        public FakeClock Clock { get; } = new();
        public FakeLoopbackListenerFactory Loopback { get; set; } =
            new(() => "?code=auth-code&state=STATE");

        public TokenStore Store => new(Path, Protector, Fs);

        public AuthManager Build(bool configured = true, CachedTokens? seed = null)
        {
            if (seed is not null)
            {
                Store.Save(seed);
            }

            return new AuthManager(
                configured ? TestTokens.Config() : null,
                TokenClient,
                Store,
                Browser,
                Loopback,
                Clock,
                TimeSpan.FromMinutes(5));
        }
    }

    // ------------------------------------------------------------------ initial state

    [Fact]
    public void Not_configured_when_no_desktop_client_json_is_available()
    {
        var auth = new Harness().Build(configured: false);

        Assert.Equal(AuthState.NotConfigured, auth.Status.State);
        Assert.False(auth.Status.CanCallBackend);
    }

    [Fact]
    public async Task Not_configured_never_yields_a_token_and_never_signs_in()
    {
        var h = new Harness();
        var auth = h.Build(configured: false);

        Assert.Null(await auth.GetValidIdTokenAsync(CancellationToken.None));
        Assert.False(await auth.SignInAsync(CancellationToken.None));
        Assert.Empty(h.Browser.OpenedUrls);
        Assert.Equal(RefreshOutcome.RefreshRevoked, await auth.ForceRefreshAsync(CancellationToken.None));
    }

    [Fact]
    public void Signed_out_when_configured_but_no_cached_refresh_token()
    {
        Assert.Equal(AuthState.SignedOut, new Harness().Build().Status.State);
    }

    [Fact]
    public void Signed_in_when_a_cached_refresh_token_exists()
    {
        var auth = new Harness().Build(seed: new CachedTokens
        {
            RefreshToken = "r",
            IdToken = TestTokens.Jwt(),
            Email = "owner@example.com",
        });

        Assert.Equal(AuthState.SignedIn, auth.Status.State);
        Assert.Equal("owner@example.com", auth.Status.Email);
    }

    // ------------------------------------------------------------------ refresh classification

    [Fact]
    public void ClassifyRefresh_maps_invalid_grant_to_revoked() =>
        Assert.Equal(RefreshOutcome.RefreshRevoked,
            AuthManager.ClassifyRefresh(new TokenResponse { Error = "invalid_grant" }));

    [Theory]
    [InlineData("server_error")]
    [InlineData("temporarily_unavailable")]
    public void ClassifyRefresh_maps_transient_errors(string error) =>
        Assert.Equal(RefreshOutcome.Transient, AuthManager.ClassifyRefresh(new TokenResponse { Error = error }));

    [Fact]
    public void ClassifyRefresh_maps_a_missing_id_token_to_NoIdToken() =>
        Assert.Equal(RefreshOutcome.NoIdToken,
            AuthManager.ClassifyRefresh(new TokenResponse { AccessToken = "at" }));

    [Fact]
    public void ClassifyRefresh_maps_a_present_id_token_to_success() =>
        Assert.Equal(RefreshOutcome.Success,
            AuthManager.ClassifyRefresh(new TokenResponse { IdToken = "id" }));

    // ------------------------------------------------------------------ token vending

    [Fact]
    public async Task Valid_cached_id_token_is_returned_without_a_refresh()
    {
        var h = new Harness();
        var auth = h.Build(seed: new CachedTokens
        {
            RefreshToken = "r",
            IdToken = TestTokens.Jwt(h.Clock.UtcNow.AddHours(1)),
        });

        var token = await auth.GetValidIdTokenAsync(CancellationToken.None);

        Assert.NotNull(token);
        Assert.Equal(0, h.TokenClient.RefreshCalls);
    }

    [Fact]
    public async Task Expiring_id_token_is_refreshed_before_expiry()
    {
        var h = new Harness();
        var fresh = TestTokens.Jwt(h.Clock.UtcNow.AddHours(1), "owner@example.com");
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse { IdToken = fresh });

        var auth = h.Build(seed: new CachedTokens
        {
            RefreshToken = "r",
            // Inside the 5-minute skew window.
            IdToken = TestTokens.Jwt(h.Clock.UtcNow.AddMinutes(2)),
        });

        var token = await auth.GetValidIdTokenAsync(CancellationToken.None);

        Assert.Equal(fresh, token);
        Assert.Equal(1, h.TokenClient.RefreshCalls);
        Assert.Equal(AuthState.SignedIn, auth.Status.State);
        Assert.Equal("owner@example.com", auth.Status.Email);
    }

    [Fact]
    public async Task Refresh_persists_a_rotated_refresh_token()
    {
        var h = new Harness();
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse
        {
            IdToken = TestTokens.Jwt(h.Clock.UtcNow.AddHours(1)),
            RefreshToken = "rotated-refresh",
        });

        var auth = h.Build(seed: new CachedTokens { RefreshToken = "old-refresh" });
        await auth.ForceRefreshAsync(CancellationToken.None);

        Assert.Equal("rotated-refresh", h.Store.Load()!.RefreshToken);
    }

    [Fact]
    public async Task Revoked_refresh_token_moves_to_sign_in_needed_and_stops_vending_tokens()
    {
        var h = new Harness();
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse { Error = "invalid_grant" });

        var auth = h.Build(seed: new CachedTokens
        {
            RefreshToken = "revoked",
            IdToken = TestTokens.Jwt(h.Clock.UtcNow.AddSeconds(10)),
        });

        var token = await auth.GetValidIdTokenAsync(CancellationToken.None);

        Assert.Null(token);
        Assert.Equal(AuthState.SignInNeeded, auth.Status.State);
        Assert.False(auth.Status.CanCallBackend);
    }

    [Fact]
    public async Task A_refresh_without_an_id_token_requires_sign_in()
    {
        var h = new Harness();
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse { AccessToken = "access-only" });

        var auth = h.Build(seed: new CachedTokens { RefreshToken = "r" });

        Assert.Equal(RefreshOutcome.NoIdToken, await auth.ForceRefreshAsync(CancellationToken.None));
        Assert.Equal(AuthState.SignInNeeded, auth.Status.State);
        // The access token must never be promoted to a backend credential.
        Assert.Null(await auth.GetValidIdTokenAsync(CancellationToken.None));
    }

    [Fact]
    public async Task A_transient_refresh_failure_keeps_the_signed_in_state()
    {
        var h = new Harness();
        h.TokenClient.RefreshThrows = new HttpRequestException("network down");

        var auth = h.Build(seed: new CachedTokens { RefreshToken = "r" });

        Assert.Equal(RefreshOutcome.Transient, await auth.ForceRefreshAsync(CancellationToken.None));
        Assert.Equal(AuthState.SignedIn, auth.Status.State);
    }

    [Fact]
    public async Task A_transient_server_error_keeps_the_signed_in_state()
    {
        var h = new Harness();
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse { Error = "server_error" });

        var auth = h.Build(seed: new CachedTokens { RefreshToken = "r" });

        Assert.Equal(RefreshOutcome.Transient, await auth.ForceRefreshAsync(CancellationToken.None));
        Assert.Equal(AuthState.SignedIn, auth.Status.State);
    }

    [Fact]
    public async Task StatusChanged_fires_on_transitions()
    {
        var h = new Harness();
        h.TokenClient.RefreshResponses.Enqueue(new TokenResponse { Error = "invalid_grant" });

        var auth = h.Build(seed: new CachedTokens { RefreshToken = "r" });
        var observed = new List<AuthState>();
        auth.StatusChanged += s => observed.Add(s.State);

        await auth.ForceRefreshAsync(CancellationToken.None);

        Assert.Contains(AuthState.SignInNeeded, observed);
    }

    // ------------------------------------------------------------------ interactive sign-in

    [Fact]
    public async Task SignInAsync_runs_the_full_pkce_flow_and_persists_tokens()
    {
        var h = BuildEchoHarness();
        var idToken = TestTokens.Jwt(h.Clock.UtcNow.AddHours(1), "owner@example.com");
        h.TokenClient.ExchangeResponses.Enqueue(new TokenResponse
        {
            IdToken = idToken,
            RefreshToken = "refresh-new",
            AccessToken = "access-should-never-be-used",
        });

        var auth = h.Build();
        var result = await auth.SignInAsync(CancellationToken.None);

        Assert.True(result);
        Assert.Equal(AuthState.SignedIn, auth.Status.State);
        Assert.Equal("owner@example.com", auth.Status.Email);
        Assert.Single(h.Browser.OpenedUrls);
        Assert.StartsWith("https://accounts.google.com/", h.Browser.OpenedUrls[0]);

        var saved = h.Store.Load();
        Assert.Equal("refresh-new", saved!.RefreshToken);
        Assert.Equal(idToken, saved.IdToken);

        // The ID token — never the access token — is the backend credential.
        Assert.Equal(idToken, await auth.GetValidIdTokenAsync(CancellationToken.None));
    }

    [Fact]
    public async Task SignInAsync_rejects_a_state_mismatch()
    {
        var h = new Harness
        {
            Loopback = new FakeLoopbackListenerFactory(() => "?code=c&state=attacker-state"),
        };
        var auth = h.Build();

        await Assert.ThrowsAsync<AuthCallbackException>(() => auth.SignInAsync(CancellationToken.None));
        Assert.Equal(0, h.TokenClient.ExchangeCalls);
    }

    [Fact]
    public async Task SignInAsync_uses_the_listener_redirect_uri_and_sends_the_verifier_once()
    {
        var h = new Harness();
        var auth = await SignInSuccessfully(h);

        Assert.Equal("http://127.0.0.1:5000/", h.TokenClient.LastRedirectUri);
        Assert.False(string.IsNullOrEmpty(h.TokenClient.LastCodeVerifier));
        Assert.InRange(h.TokenClient.LastCodeVerifier!.Length, 43, 128);
        Assert.Single(h.Loopback.Created);
        Assert.True(h.Loopback.Created[0].Disposed);
        Assert.Equal(AuthState.SignedIn, auth.Status.State);
    }

    [Fact]
    public async Task SignInAsync_fails_when_no_refresh_token_is_returned()
    {
        var h = BuildEchoHarness();
        h.TokenClient.ExchangeResponses.Enqueue(new TokenResponse { IdToken = TestTokens.Jwt() });

        var auth = h.Build();
        Assert.False(await auth.SignInAsync(CancellationToken.None));
        Assert.Equal(AuthState.SignInNeeded, auth.Status.State);
    }

    [Fact]
    public async Task SignOut_deletes_the_cached_tokens()
    {
        var h = new Harness();
        var auth = h.Build(seed: new CachedTokens { RefreshToken = "r", IdToken = TestTokens.Jwt() });

        auth.SignOut();

        Assert.Equal(AuthState.SignedOut, auth.Status.State);
        Assert.Null(h.Store.Load());
        Assert.False(h.Fs.FileExists(Path));
        Assert.Null(await auth.GetValidIdTokenAsync(CancellationToken.None));
    }

    // ------------------------------------------------------------------ helpers

    /// <summary>
    /// A harness whose loopback listener echoes back whatever <c>state</c> the authorization
    /// URL contained, simulating a well-behaved browser round trip.
    /// </summary>
    private static Harness BuildEchoHarness()
    {
        var h = new Harness();
        h.Loopback = new FakeLoopbackListenerFactory(() =>
        {
            var url = h.Browser.OpenedUrls[^1];
            var state = System.Web.HttpUtility.ParseQueryString(new Uri(url).Query)["state"];
            return $"?code=auth-code&state={state}";
        });
        return h;
    }

    private static async Task<AuthManager> SignInSuccessfully(Harness h)
    {
        h.Loopback = new FakeLoopbackListenerFactory(() =>
        {
            var url = h.Browser.OpenedUrls[^1];
            var state = System.Web.HttpUtility.ParseQueryString(new Uri(url).Query)["state"];
            return $"?code=auth-code&state={state}";
        });

        h.TokenClient.ExchangeResponses.Enqueue(new TokenResponse
        {
            IdToken = TestTokens.Jwt(h.Clock.UtcNow.AddHours(1)),
            RefreshToken = "refresh-new",
        });

        var auth = h.Build();
        Assert.True(await auth.SignInAsync(CancellationToken.None));
        return auth;
    }
}
