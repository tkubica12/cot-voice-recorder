using System.Net;
using System.Text.Json;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Api;

public class ApiExceptionTests
{
    [Theory]
    [InlineData(400, ApiErrorKind.BadRequest)]
    [InlineData(401, ApiErrorKind.Unauthorized)]
    [InlineData(403, ApiErrorKind.Forbidden)]
    [InlineData(404, ApiErrorKind.NotFound)]
    [InlineData(408, ApiErrorKind.Retryable)]
    [InlineData(409, ApiErrorKind.Unexpected)]
    [InlineData(422, ApiErrorKind.Unexpected)]
    [InlineData(429, ApiErrorKind.Retryable)]
    [InlineData(500, ApiErrorKind.Retryable)]
    [InlineData(503, ApiErrorKind.Retryable)]
    [InlineData(599, ApiErrorKind.Retryable)]
    [InlineData(302, ApiErrorKind.Unexpected)]
    public void Classify_maps_status_codes(int status, ApiErrorKind expected) =>
        Assert.Equal(expected, ApiException.Classify(status));

    [Fact]
    public void FromResponse_parses_an_rfc9457_problem_body()
    {
        const string body = """
        {
          "type": "https://voice-recorder/errors/not-found",
          "title": "Not found",
          "status": 404,
          "detail": "Transcript not found or expired.",
          "instance": "/v1/transcripts/abc"
        }
        """;

        var ex = ApiException.FromResponse(404, body);

        Assert.Equal(ApiErrorKind.NotFound, ex.Kind);
        Assert.Equal(404, ex.StatusCode);
        Assert.NotNull(ex.Problem);
        Assert.Equal("Not found", ex.Problem!.Title);
        Assert.Equal("https://voice-recorder/errors/not-found", ex.Problem.Type);
        Assert.Equal(404, ex.Problem.Status);
        Assert.Equal("Transcript not found or expired.", ex.Problem.Detail);
        Assert.Equal("/v1/transcripts/abc", ex.Problem.Instance);
        Assert.Contains("Not found", ex.Message);
    }

    [Fact]
    public void FromResponse_tolerates_a_non_problem_body()
    {
        var ex = ApiException.FromResponse(500, "<html>gateway error</html>");

        Assert.Equal(ApiErrorKind.Retryable, ex.Kind);
        Assert.Null(ex.Problem);
    }

    [Fact]
    public void FromResponse_tolerates_an_empty_body()
    {
        var ex = ApiException.FromResponse(403, null);

        Assert.Equal(ApiErrorKind.Forbidden, ex.Kind);
        Assert.Null(ex.Problem);
    }
}

public class ApiClientTests
{
    private const string TranscriptJson = """
    {
      "transcript_id": "1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f",
      "recording_id": "9a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
      "body": "Připomeň mi zítra ráno zavolat doktorovi ohledně výsledků.",
      "preview": "Připomeň mi zítra ráno zavolat doktorovi…",
      "language": "cs",
      "refine_model": "gpt-5.6-luna",
      "completed_at": "2026-09-03T13:53:10Z",
      "expires_at": "2026-09-05T13:53:10Z",
      "character_count": 58
    }
    """;

    private const string NegotiateJson = """
    {
      "url": "wss://voice-recorder.webpubsub.azure.com/client/hubs/transcripts?access_token=abc",
      "hub": "transcripts",
      "group": "user-allowlisted",
      "expires_at": "2026-09-03T14:52:00Z"
    }
    """;

    private static (ApiClient Client, FakeHttpMessageHandler Handler, FakeCredentials Creds) Build(
        FakeHttpMessageHandler? handler = null, FakeCredentials? creds = null)
    {
        handler ??= new FakeHttpMessageHandler();
        creds ??= new FakeCredentials();
        var http = new HttpClient(handler) { BaseAddress = new Uri("https://api.example.test/") };
        var retry = new ApiRetryOptions { MaxRetries = 3, Backoff = _ => TimeSpan.Zero };
        return (new ApiClient(http, creds, retry, (_, _) => Task.CompletedTask), handler, creds);
    }

    // ------------------------------------------------------------------ happy paths

    [Fact]
    public async Task Dictation_refinement_sends_only_current_text_and_context_with_existing_auth()
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.OK, """{"text":"Keep port 8443.","edits":[{"original":"Keep keep","replacement":"Keep"}]}"""));
        var result = await client.RefineDictationAsync("Keep keep port 8443.", "Previous sentence.", CancellationToken.None);
        Assert.Equal("Keep port 8443.", result.Text);
        Assert.Equal("Keep keep", Assert.Single(result.Edits).Original);
        Assert.Equal("Keep", result.Edits[0].Replacement);
        Assert.Equal("/v1/dictation/refine", handler.Requests.Single().RequestUri!.AbsolutePath);
        Assert.Equal(HttpMethod.Post, handler.Requests.Single().Method);
        Assert.Equal("token-1", handler.AuthorizationValues.Single());
        using var body = JsonDocument.Parse(handler.RequestBodies.Single());
        Assert.Equal(2, body.RootElement.EnumerateObject().Count());
        Assert.Equal("Keep keep port 8443.", body.RootElement.GetProperty("text").GetString());
        Assert.Equal("Previous sentence.", body.RootElement.GetProperty("previous_text").GetString());
    }

    [Theory]
    [InlineData(HttpStatusCode.TooManyRequests)]
    [InlineData(HttpStatusCode.InternalServerError)]
    [InlineData(HttpStatusCode.NotFound)]
    public async Task Optional_refinement_never_retries_model_or_missing_route_errors(HttpStatusCode status)
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler().EnqueueJson(status, "{}"));
        await Assert.ThrowsAsync<ApiException>(() => client.RefineDictationAsync("Raw text.", "", CancellationToken.None));
        Assert.Single(handler.Requests);
    }

    [Theory]
    [InlineData("{}")]
    [InlineData("""{"text":null}""")]
    [InlineData("""{"text":""}""")]
    [InlineData("""{"text":"  "}""")]
    [InlineData("""{"text":"Raw text."}""")]
    [InlineData("""{"text":"Raw text.","edits":null}""")]
    [InlineData("""{"text":"Raw text.","edits":[null]}""")]
    [InlineData("""{"text":"Raw text.","edits":[{"original":"","replacement":"x"}]}""")]
    [InlineData("""{"text":"Raw text.","edits":[{"original":"Raw","replacement":null}]}""")]
    public async Task Refinement_rejects_missing_or_empty_results(string response)
    {
        var (client, _, _) = Build(new FakeHttpMessageHandler().EnqueueJson(HttpStatusCode.OK, response));
        await Assert.ThrowsAsync<ApiException>(() => client.RefineDictationAsync("Raw text.", "", CancellationToken.None));
    }

    [Fact]
    public async Task Refinement_bounds_inputs_and_obeys_cancellation_before_sending()
    {
        var (client, handler, _) = Build();
        await Assert.ThrowsAsync<ArgumentException>(() => client.RefineDictationAsync(new string('x', 4001), "", CancellationToken.None));
        await Assert.ThrowsAsync<ArgumentException>(() => client.RefineDictationAsync("text", new string('x', 8001), CancellationToken.None));
        await Assert.ThrowsAsync<ArgumentException>(() => client.RefineDictationAsync(" ", "", CancellationToken.None));
        await Assert.ThrowsAsync<ArgumentException>(() => client.RefineDictationAsync(
            new string('\u4e00', 4000), new string('\u4e00', 8000), CancellationToken.None));
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => client.RefineDictationAsync("text", "", cancel.Token));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task Refinement_retains_one_401_refresh_without_transient_retries()
    {
        var (client, handler, credentials) = Build(new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.Unauthorized, "{}")
            .EnqueueJson(HttpStatusCode.OK, """{"text":"Clean text.","edits":[{"original":"Raw","replacement":"Clean"}]}"""));
        Assert.Equal("Clean text.", (await client.RefineDictationAsync("Raw text.", "", CancellationToken.None)).Text);
        Assert.Equal(2, handler.Requests.Count);
        Assert.Equal(handler.RequestBodies[0], handler.RequestBodies[1]);
        Assert.Equal(1, credentials.RefreshCalls);
    }

    [Fact]
    public async Task Refinement_does_not_retry_transport_failures()
    {
        foreach (var error in new Exception[] { new HttpRequestException("Offline"), new TaskCanceledException("Timeout") })
        {
            var (client, handler, _) = Build(new FakeHttpMessageHandler().EnqueueThrow(error));
            await Assert.ThrowsAsync<ApiException>(() => client.RefineDictationAsync("Raw text.", "", CancellationToken.None));
            Assert.Single(handler.Requests);
        }
    }

    [Fact]
    public async Task GetTranscriptAsync_deserialises_the_openapi_shape()
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler().EnqueueJson(HttpStatusCode.OK, TranscriptJson));

        var transcript = await client.GetTranscriptAsync(
            "1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f", CancellationToken.None);

        Assert.Equal("1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f", transcript.TranscriptId);
        Assert.Equal(58, transcript.CharacterCount);
        Assert.Equal("cs", transcript.Language);
        Assert.StartsWith("Připomeň", transcript.Body);
        Assert.Equal(
            "/v1/transcripts/1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f",
            handler.Requests[0].RequestUri!.AbsolutePath);
    }

    [Fact]
    public async Task Requests_carry_the_id_token_as_a_bearer()
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler().EnqueueJson(HttpStatusCode.OK, TranscriptJson));

        await client.GetTranscriptAsync("t1", CancellationToken.None);

        Assert.Equal("Bearer", handler.Requests[0].Headers.Authorization!.Scheme);
        Assert.Equal("token-1", handler.Requests[0].Headers.Authorization!.Parameter);
    }

    [Fact]
    public async Task NegotiateAsync_posts_client_info_and_reads_the_access_url()
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler().EnqueueJson(HttpStatusCode.OK, NegotiateJson));

        var negotiate = await client.NegotiateAsync("windows", "1.0.0", CancellationToken.None);

        Assert.Equal("transcripts", negotiate.Hub);
        Assert.Equal("user-allowlisted", negotiate.Group);
        Assert.StartsWith("wss://", negotiate.Url);
        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Equal("/v1/realtime/negotiate", handler.Requests[0].RequestUri!.AbsolutePath);

        var body = handler.RequestBodies[0];
        Assert.Contains("\"platform\":\"windows\"", body);
        Assert.Contains("\"app_version\":\"1.0.0\"", body);
    }

    [Fact]
    public async Task ListTranscriptsAsync_builds_the_cursor_query()
    {
        var (client, handler, _) = Build(new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.OK, """{"items":[],"next_cursor":null}"""));

        var page = await client.ListTranscriptsAsync("cur sor", 25, CancellationToken.None);

        Assert.Empty(page.Items);
        Assert.Null(page.NextCursor);
        Assert.Equal("?cursor=cur%20sor&limit=25", handler.Requests[0].RequestUri!.Query);
    }

    // ------------------------------------------------------------------ auth behaviour

    [Fact]
    public async Task Throws_unauthorized_without_a_token_and_never_calls_the_backend()
    {
        var (client, handler, _) = Build(creds: new FakeCredentials(token: null));

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Unauthorized, ex.Kind);
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task Refreshes_exactly_once_on_401_and_retries_with_the_new_token()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""")
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);
        var creds = new FakeCredentials();
        var (client, _, _) = Build(handler, creds);

        var transcript = await client.GetTranscriptAsync("t", CancellationToken.None);

        Assert.Equal(58, transcript.CharacterCount);
        Assert.Equal(1, creds.RefreshCalls);
        Assert.Equal(new[] { "token-1", "token-2" }, handler.AuthorizationValues);
    }

    [Fact]
    public async Task Does_not_refresh_more_than_once_for_repeated_401s()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""")
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""");
        var creds = new FakeCredentials();
        var (client, _, _) = Build(handler, creds);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Unauthorized, ex.Kind);
        Assert.Equal(1, creds.RefreshCalls);
        Assert.Equal(2, handler.Requests.Count);
    }

    [Fact]
    public async Task Surfaces_401_immediately_when_the_refresh_fails()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueProblem(HttpStatusCode.Unauthorized, """{"title":"Unauthorized","status":401}""");
        var creds = new FakeCredentials { RefreshSucceeds = false };
        var (client, _, _) = Build(handler, creds);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Unauthorized, ex.Kind);
        Assert.Equal(1, creds.RefreshCalls);
        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task A_403_is_terminal_and_never_refreshed_or_retried()
    {
        var handler = new FakeHttpMessageHandler().EnqueueProblem(
            HttpStatusCode.Forbidden,
            """{"type":"https://voice-recorder/errors/forbidden","title":"Forbidden","status":403,"detail":"Account is not allowlisted."}""");
        var creds = new FakeCredentials();
        var (client, _, _) = Build(handler, creds);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Forbidden, ex.Kind);
        Assert.Equal("Account is not allowlisted.", ex.Problem!.Detail);
        Assert.Equal(0, creds.RefreshCalls);
        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task A_404_is_terminal()
    {
        var handler = new FakeHttpMessageHandler().EnqueueProblem(
            HttpStatusCode.NotFound, """{"title":"Not found","status":404}""");
        var (client, _, _) = Build(handler);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.NotFound, ex.Kind);
        Assert.Single(handler.Requests);
    }

    // ------------------------------------------------------------------ retry behaviour

    [Fact]
    public async Task Retries_a_429_with_backoff_and_then_succeeds()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueJson(HttpStatusCode.TooManyRequests, """{"title":"Too many requests","status":429}""")
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);
        var (client, _, _) = Build(handler);

        var transcript = await client.GetTranscriptAsync("t", CancellationToken.None);

        Assert.Equal(58, transcript.CharacterCount);
        Assert.Equal(2, handler.Requests.Count);
    }

    [Fact]
    public async Task Retries_5xx_up_to_the_bound_then_throws_retryable()
    {
        var handler = new FakeHttpMessageHandler();
        for (var i = 0; i < 6; i++)
        {
            handler.EnqueueJson(HttpStatusCode.ServiceUnavailable, """{"title":"unavailable","status":503}""");
        }

        var (client, _, _) = Build(handler);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Retryable, ex.Kind);
        // 1 initial + 3 retries.
        Assert.Equal(4, handler.Requests.Count);
    }

    [Fact]
    public async Task Retries_network_errors_then_throws_retryable()
    {
        var handler = new FakeHttpMessageHandler();
        for (var i = 0; i < 6; i++)
        {
            handler.EnqueueThrow(new HttpRequestException("connection reset"));
        }

        var (client, _, _) = Build(handler);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Retryable, ex.Kind);
        Assert.Equal(4, handler.Requests.Count);
    }

    [Fact]
    public async Task Recovers_after_a_transient_network_error()
    {
        var handler = new FakeHttpMessageHandler()
            .EnqueueThrow(new HttpRequestException("connection reset"))
            .EnqueueJson(HttpStatusCode.OK, TranscriptJson);
        var (client, _, _) = Build(handler);

        Assert.Equal(58, (await client.GetTranscriptAsync("t", CancellationToken.None)).CharacterCount);
    }

    [Fact]
    public async Task A_timeout_is_retried_and_finally_reported_as_408()
    {
        var handler = new FakeHttpMessageHandler();
        for (var i = 0; i < 6; i++)
        {
            handler.EnqueueThrow(new TaskCanceledException("timeout"));
        }

        var (client, _, _) = Build(handler);

        var ex = await Assert.ThrowsAsync<ApiException>(
            () => client.GetTranscriptAsync("t", CancellationToken.None));

        Assert.Equal(ApiErrorKind.Retryable, ex.Kind);
        Assert.Equal(408, ex.StatusCode);
    }

    [Fact]
    public async Task Honours_caller_cancellation()
    {
        using var cts = new CancellationTokenSource();
        await cts.CancelAsync();
        var (client, handler, _) = Build();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => client.GetTranscriptAsync("t", cts.Token));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public void Default_backoff_grows_and_is_bounded()
    {
        var options = new ApiRetryOptions();

        Assert.True(options.Backoff(1) < options.Backoff(2));
        Assert.True(options.Backoff(2) < options.Backoff(3));
        Assert.True(options.Backoff(20) <= TimeSpan.FromMilliseconds(4000));
    }
}
