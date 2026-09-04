using System.Security.Cryptography;
using System.Text;
using System.Web;
using VoicePrompt.Core.Auth;
using Xunit;

namespace VoicePrompt.Core.Tests.Auth;

public class PkceAndAuthUrlTests
{
    [Fact]
    public void Create_produces_a_valid_rfc7636_verifier_and_s256_challenge()
    {
        var pkce = PkceGenerator.Create();

        Assert.InRange(pkce.CodeVerifier.Length, 43, 128);
        Assert.Equal("S256", pkce.CodeChallengeMethod);
        Assert.Matches("^[A-Za-z0-9._~-]+$", pkce.CodeVerifier);
        Assert.DoesNotContain('=', pkce.CodeChallenge);
        Assert.DoesNotContain('+', pkce.CodeChallenge);
        Assert.DoesNotContain('/', pkce.CodeChallenge);

        var expected = PkceGenerator.Base64Url(SHA256.HashData(Encoding.ASCII.GetBytes(pkce.CodeVerifier)));
        Assert.Equal(expected, pkce.CodeChallenge);
    }

    [Fact]
    public void Create_produces_unique_verifiers_and_states()
    {
        var verifiers = new HashSet<string>();
        var states = new HashSet<string>();
        for (var i = 0; i < 64; i++)
        {
            var pkce = PkceGenerator.Create();
            Assert.True(verifiers.Add(pkce.CodeVerifier));
            Assert.True(states.Add(pkce.State));
        }
    }

    [Theory]
    [InlineData(16)]
    [InlineData(128)]
    public void Create_rejects_out_of_range_verifier_sizes(int bytes) =>
        Assert.Throws<ArgumentOutOfRangeException>(() => PkceGenerator.Create(bytes));

    [Fact]
    public void Challenge_matches_the_rfc7636_appendix_b_vector()
    {
        // RFC 7636 Appendix B.
        const string verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        Assert.Equal("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", PkceGenerator.Challenge(verifier));
    }

    [Fact]
    public void Authorization_url_carries_pkce_offline_access_and_minimal_scopes()
    {
        var config = TestTokens.Config();
        var pkce = PkceGenerator.Create();

        var url = AuthorizationRequestBuilder.BuildAuthorizationUrl(
            config, "http://127.0.0.1:51234/", pkce);

        var uri = new Uri(url);
        var q = HttpUtility.ParseQueryString(uri.Query);

        Assert.Equal("accounts.google.com", uri.Host);
        Assert.Equal(config.ClientId, q["client_id"]);
        Assert.Equal("code", q["response_type"]);
        Assert.Equal("http://127.0.0.1:51234/", q["redirect_uri"]);
        Assert.Equal("openid email profile", q["scope"]);
        Assert.Equal(pkce.CodeChallenge, q["code_challenge"]);
        Assert.Equal("S256", q["code_challenge_method"]);
        Assert.Equal(pkce.State, q["state"]);
        Assert.Equal("offline", q["access_type"]);
        Assert.Equal("consent", q["prompt"]);

        // The verifier must never travel in the browser request.
        Assert.DoesNotContain(pkce.CodeVerifier, url);
        // A public desktop client secret must never be put in the authorization URL.
        Assert.DoesNotContain(config.ClientSecret, url);
    }

    [Fact]
    public void Authorization_url_includes_login_hint_when_supplied()
    {
        var url = AuthorizationRequestBuilder.BuildAuthorizationUrl(
            TestTokens.Config(), "http://127.0.0.1:1/", PkceGenerator.Create(), loginHint: "owner@example.com");

        Assert.Equal("owner@example.com", HttpUtility.ParseQueryString(new Uri(url).Query)["login_hint"]);
    }

    [Fact]
    public void Callback_returns_the_code_when_state_matches()
    {
        var code = AuthorizationRequestBuilder.ParseAndValidateCallback(
            "?code=abc123&state=expected-state&scope=openid", "expected-state");

        Assert.Equal("abc123", code);
    }

    [Fact]
    public void Callback_accepts_a_query_without_a_leading_question_mark()
    {
        var code = AuthorizationRequestBuilder.ParseAndValidateCallback(
            "code=abc123&state=s", "s");

        Assert.Equal("abc123", code);
    }

    [Theory]
    [InlineData("?code=abc&state=attacker", "expected")]
    [InlineData("?code=abc", "expected")]
    [InlineData("?code=abc&state=", "expected")]
    [InlineData("?code=abc&state=expecte", "expected")]
    public void Callback_rejects_a_state_mismatch(string query, string expectedState) =>
        Assert.Throws<AuthCallbackException>(
            () => AuthorizationRequestBuilder.ParseAndValidateCallback(query, expectedState));

    [Fact]
    public void Callback_rejects_an_oauth_error_response()
    {
        var ex = Assert.Throws<AuthCallbackException>(() =>
            AuthorizationRequestBuilder.ParseAndValidateCallback("?error=access_denied&state=s", "s"));

        Assert.Contains("access_denied", ex.Message);
    }

    [Fact]
    public void Callback_rejects_a_missing_code()
    {
        var ex = Assert.Throws<AuthCallbackException>(() =>
            AuthorizationRequestBuilder.ParseAndValidateCallback("?state=s", "s"));

        Assert.Contains("did not include a code", ex.Message);
    }
}
