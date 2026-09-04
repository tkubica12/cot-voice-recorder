using System.Net;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Auth;

public class DesktopClientConfigTests
{
    [Fact]
    public void Parse_reads_an_installed_desktop_client()
    {
        var config = DesktopClientConfig.Parse(TestTokens.DesktopClientJson);

        Assert.Equal("test-desktop-client.apps.googleusercontent.com", config.ClientId);
        Assert.Equal("test-not-a-real-secret", config.ClientSecret);
        Assert.Equal("https://accounts.google.com/o/oauth2/auth", config.AuthUri);
        Assert.Equal("https://oauth2.googleapis.com/token", config.TokenUri);
    }

    [Fact]
    public void Scopes_are_limited_to_openid_email_profile() =>
        Assert.Equal(new[] { "openid", "email", "profile" }, DesktopClientConfig.Scopes);

    [Fact]
    public void LooksLikeDesktopClient_only_accepts_installed_plus_secret()
    {
        Assert.True(DesktopClientConfig.LooksLikeDesktopClient(TestTokens.DesktopClientJson));
        // Android: installed root without a client secret.
        Assert.False(DesktopClientConfig.LooksLikeDesktopClient(TestTokens.AndroidClientJson));
        // Web: wrong root.
        Assert.False(DesktopClientConfig.LooksLikeDesktopClient(TestTokens.WebClientJson));
        Assert.False(DesktopClientConfig.LooksLikeDesktopClient("not json"));
        Assert.False(DesktopClientConfig.LooksLikeDesktopClient("{}"));
    }

    [Fact]
    public void Parse_throws_without_a_recognised_root() =>
        Assert.Throws<FormatException>(() => DesktopClientConfig.Parse("""{"other":{}}"""));

    [Fact]
    public void Parse_throws_when_the_client_secret_is_missing() =>
        Assert.Throws<FormatException>(() => DesktopClientConfig.Parse(TestTokens.AndroidClientJson));
}

public class DesktopClientLocatorTests
{
    private const string Base = @"C:\Program Files\VoicePrompt";
    private const string State = @"C:\Users\me\AppData\Local\VoicePrompt";

    [Fact]
    public void Returns_null_and_stays_safe_when_nothing_is_configured()
    {
        var log = new RecordingLog();

        var config = DesktopClientLocator.TryLoad(Base, State, new FakeFileSystem(), log);

        Assert.Null(config);
        Assert.Contains("sign-in disabled", log.All);
    }

    [Fact]
    public void Prefers_the_user_state_directory_over_the_install_directory()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path.Combine(State, DesktopClientLocator.FileName), TestTokens.DesktopClientJson);
        fs.Seed(Path.Combine(Base, DesktopClientLocator.FileName), TestTokens.WebClientJson);

        var config = DesktopClientLocator.TryLoad(Base, State, fs);

        Assert.NotNull(config);
        Assert.Equal("test-desktop-client.apps.googleusercontent.com", config!.ClientId);
    }

    [Fact]
    public void Finds_the_json_next_to_the_executable()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path.Combine(Base, DesktopClientLocator.FileName), TestTokens.DesktopClientJson);

        Assert.NotNull(DesktopClientLocator.TryLoad(Base, State, fs));
    }

    [Fact]
    public void Ignores_a_non_desktop_client_json()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path.Combine(Base, DesktopClientLocator.FileName), TestTokens.AndroidClientJson);
        var log = new RecordingLog();

        Assert.Null(DesktopClientLocator.TryLoad(Base, State, fs, log));
        Assert.Contains("not a Desktop client", log.All);
    }

    [Fact]
    public void Never_logs_the_client_id_or_secret()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path.Combine(State, DesktopClientLocator.FileName), TestTokens.DesktopClientJson);
        var log = new RecordingLog();

        DesktopClientLocator.TryLoad(Base, State, fs, log);

        Assert.DoesNotContain("test-not-a-real-secret", log.All);
        Assert.DoesNotContain("test-desktop-client", log.All);
    }

    [Fact]
    public void Candidate_paths_include_the_repo_staging_folder_for_dev_runs()
    {
        var candidates = DesktopClientLocator
            .CandidatePaths(@"D:\repo\windows\src\VoicePrompt.App\bin\Release", State)
            .ToList();

        Assert.Contains(candidates, p => p.Contains(Path.Combine("installer", "staging")));
    }
}

public class GoogleTokenClientTests
{
    [Fact]
    public async Task ExchangeCodeAsync_posts_the_authorization_code_grant()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.OK, """{"id_token":"id","refresh_token":"r","expires_in":3599}""");
        using var http = new HttpClient(handler);
        var client = new GoogleTokenClient(http);

        var response = await client.ExchangeCodeAsync(
            TestTokens.Config(), "the-code", "the-verifier", "http://127.0.0.1:1/", CancellationToken.None);

        Assert.Equal("id", response.IdToken);
        Assert.Equal("r", response.RefreshToken);

        var body = handler.RequestBodies[0];
        Assert.Contains("grant_type=authorization_code", body);
        Assert.Contains("code=the-code", body);
        Assert.Contains("code_verifier=the-verifier", body);
        Assert.Contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2F", body);
    }

    [Fact]
    public async Task RefreshAsync_posts_the_refresh_token_grant()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.OK, """{"id_token":"id2"}""");
        using var http = new HttpClient(handler);

        var response = await new GoogleTokenClient(http)
            .RefreshAsync(TestTokens.Config(), "the-refresh", CancellationToken.None);

        Assert.Equal("id2", response.IdToken);

        var body = handler.RequestBodies[0];
        Assert.Contains("grant_type=refresh_token", body);
        Assert.Contains("refresh_token=the-refresh", body);
    }

    [Fact]
    public async Task A_400_invalid_grant_body_is_surfaced_for_classification()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.BadRequest, """{"error":"invalid_grant","error_description":"Token has been expired or revoked."}""");
        using var http = new HttpClient(handler);

        var response = await new GoogleTokenClient(http)
            .RefreshAsync(TestTokens.Config(), "r", CancellationToken.None);

        Assert.Equal("invalid_grant", response.Error);
        Assert.Equal(RefreshOutcome.RefreshRevoked, AuthManager.ClassifyRefresh(response));
    }

    [Fact]
    public async Task A_5xx_without_a_body_becomes_a_transient_server_error()
    {
        var handler = new FakeHttpMessageHandler()
            .Enqueue(_ => new HttpResponseMessage(HttpStatusCode.BadGateway));
        using var http = new HttpClient(handler);

        var response = await new GoogleTokenClient(http)
            .RefreshAsync(TestTokens.Config(), "r", CancellationToken.None);

        Assert.Equal("server_error", response.Error);
        Assert.Equal(RefreshOutcome.Transient, AuthManager.ClassifyRefresh(response));
    }
}

public class AuthBackendCredentialsTests
{
    [Fact]
    public async Task Hands_out_the_id_token_and_refreshes_once_on_401()
    {
        var fs = new FakeFileSystem();
        var clock = new FakeClock();
        var tokenClient = new FakeGoogleTokenClient();
        var store = new TokenStore(@"C:\t.bin", new FakeSecretProtector(), fs);
        store.Save(new CachedTokens { RefreshToken = "r", IdToken = TestTokens.Jwt(clock.UtcNow.AddHours(1)) });

        tokenClient.RefreshResponses.Enqueue(new TokenResponse { IdToken = TestTokens.Jwt(clock.UtcNow.AddHours(2)) });

        var auth = new AuthManager(
            TestTokens.Config(), tokenClient, store, new FakeBrowser(),
            new FakeLoopbackListenerFactory(() => ""), clock);

        var credentials = new AuthBackendCredentials(auth);

        Assert.NotNull(await credentials.GetIdTokenAsync(CancellationToken.None));
        Assert.True(await credentials.TryRefreshAfterUnauthorizedAsync(CancellationToken.None));
        Assert.Equal(1, tokenClient.RefreshCalls);
    }

    [Fact]
    public async Task Reports_failure_when_the_refresh_is_revoked()
    {
        var fs = new FakeFileSystem();
        var clock = new FakeClock();
        var tokenClient = new FakeGoogleTokenClient();
        var store = new TokenStore(@"C:\t.bin", new FakeSecretProtector(), fs);
        store.Save(new CachedTokens { RefreshToken = "r" });
        tokenClient.RefreshResponses.Enqueue(new TokenResponse { Error = "invalid_grant" });

        var auth = new AuthManager(
            TestTokens.Config(), tokenClient, store, new FakeBrowser(),
            new FakeLoopbackListenerFactory(() => ""), clock);

        Assert.False(await new AuthBackendCredentials(auth).TryRefreshAfterUnauthorizedAsync(CancellationToken.None));
        Assert.Equal(AuthState.SignInNeeded, auth.Status.State);
    }
}
