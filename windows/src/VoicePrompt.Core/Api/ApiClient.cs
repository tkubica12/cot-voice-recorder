using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Encodings.Web;
using System.Text.Unicode;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Dictation;

namespace VoicePrompt.Core.Api;

/// <summary>Tunable retry behavior for transient (429/5xx/timeout) responses.</summary>
public sealed class ApiRetryOptions
{
    public int MaxRetries { get; init; } = 3;
    public Func<int, TimeSpan> Backoff { get; init; } = attempt =>
        TimeSpan.FromMilliseconds(Math.Min(4000, 250 * Math.Pow(2, attempt)));
}

/// <summary>
/// Typed client for the voice-recorder backend. Attaches a Google ID token as the bearer,
/// classifies RFC 9457 problem responses, refreshes once on 401 then surfaces sign-in-needed,
/// and retries 429/5xx/timeouts with bounded backoff. Never logs tokens or transcript bodies.
/// </summary>
public sealed class ApiClient
{
    private static readonly JsonSerializerOptions DictationJson = new()
    {
        Encoder = JavaScriptEncoder.Create(UnicodeRanges.All),
    };
    private readonly HttpClient _http;
    private readonly IBackendCredentials _credentials;
    private readonly ApiRetryOptions _retry;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;
    private readonly Func<Uri>? _baseUrl;

    public ApiClient(
        HttpClient http,
        IBackendCredentials credentials,
        ApiRetryOptions? retry = null,
        Func<TimeSpan, CancellationToken, Task>? delay = null,
        Func<Uri>? baseUrl = null)
    {
        _http = http;
        _credentials = credentials;
        _retry = retry ?? new ApiRetryOptions();
        _delay = delay ?? Task.Delay;
        _baseUrl = baseUrl;
    }

    public async Task<string> TranscribeDictationAsync(byte[] wav, string language, CancellationToken ct)
    {
        var result = await SendAsync<DictationResponse>(
            HttpMethod.Post, "/v1/dictation/transcribe?language=" + Uri.EscapeDataString(language),
            null, ct, () =>
            {
                var content = new ByteArrayContent(wav);
                content.Headers.ContentType = new MediaTypeHeaderValue("audio/wav");
                return content;
            }).ConfigureAwait(false);
        return result.Text ?? throw new ApiException(
            ApiErrorKind.Unexpected, 200, null, "Missing dictation text.");
    }

    private sealed class DictationResponse
    {
        [System.Text.Json.Serialization.JsonPropertyName("text")]
        public string? Text { get; init; }
    }

    private sealed class RefinementResponse
    {
        [System.Text.Json.Serialization.JsonPropertyName("text")]
        public string? Text { get; init; }
        [System.Text.Json.Serialization.JsonPropertyName("edits")]
        public RefinementEditResponse?[]? Edits { get; init; }
    }

    private sealed class RefinementEditResponse
    {
        [System.Text.Json.Serialization.JsonPropertyName("original")]
        public string? Original { get; init; }
        [System.Text.Json.Serialization.JsonPropertyName("replacement")]
        public string? Replacement { get; init; }
    }

    public async Task<DictationRefinement> RefineDictationAsync(string text, string previousText, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(text) || text.Length > 4000)
            throw new ArgumentException("Dictation refinement requires a nonempty block of at most 4000 characters.", nameof(text));
        if (previousText is null || previousText.Length > 8000)
            throw new ArgumentException("Dictation context must be at most 8000 characters.", nameof(previousText));
        var payload = JsonSerializer.Serialize(new { text, previous_text = previousText }, DictationJson);
        if (Encoding.UTF8.GetByteCount(payload) > 32 * 1024)
            throw new ArgumentException("Dictation refinement payload exceeds 32 KiB.", nameof(text));
        var result = await SendAsync<RefinementResponse>(
            HttpMethod.Post, "/v1/dictation/refine", payload, ct, maxRetries: 0).ConfigureAwait(false);
        if (string.IsNullOrWhiteSpace(result.Text) || result.Text.Length > 4000
            || result.Edits is null || result.Edits.Length > 64)
            throw new ApiException(ApiErrorKind.Unexpected, 200, null, "Invalid dictation refinement patch response. Update the backend.");
        var edits = new List<DictationEdit>();
        foreach (var edit in result.Edits)
        {
            if (edit is null || string.IsNullOrEmpty(edit.Original) || edit.Original.Length > 512
                || edit.Replacement is null || edit.Replacement.Length > 512)
                throw new ApiException(ApiErrorKind.Unexpected, 200, null, "Invalid dictation refinement edit.");
            edits.Add(new(edit.Original, edit.Replacement));
        }
        return new(result.Text, edits);
    }

    public async Task<NegotiateResponse> NegotiateAsync(string? platform, string? appVersion, CancellationToken ct)
    {
        var payload = JsonSerializer.Serialize(new
        {
            client = new { platform, app_version = appVersion },
        });
        return await SendAsync<NegotiateResponse>(
            HttpMethod.Post, "/v1/realtime/negotiate", payload, ct).ConfigureAwait(false);
    }

    public Task<Transcript> GetTranscriptAsync(string transcriptId, CancellationToken ct) =>
        SendAsync<Transcript>(HttpMethod.Get, $"/v1/transcripts/{Uri.EscapeDataString(transcriptId)}", null, ct);

    /// <summary>
    /// <c>GET /v1/transcripts</c> — one page of transcript summaries (newest first). Previews
    /// only; the full body is fetched per transcript on demand.
    /// </summary>
    public Task<TranscriptListPage> ListTranscriptsAsync(string? cursor, int? limit, CancellationToken ct)
    {
        var query = new List<string>(2);
        if (!string.IsNullOrEmpty(cursor))
        {
            query.Add("cursor=" + Uri.EscapeDataString(cursor));
        }

        if (limit is > 0)
        {
            query.Add("limit=" + limit.Value.ToString(System.Globalization.CultureInfo.InvariantCulture));
        }

        var path = "/v1/transcripts" + (query.Count > 0 ? "?" + string.Join('&', query) : string.Empty);
        return SendAsync<TranscriptListPage>(HttpMethod.Get, path, null, ct);
    }

    private async Task<T> SendAsync<T>(
        HttpMethod method, string path, string? jsonBody, CancellationToken ct,
        Func<HttpContent>? bodyFactory = null, int? maxRetries = null)
    {
        var refreshedOnce = false;
        var attempt = 0;
        var retryLimit = maxRetries ?? _retry.MaxRetries;

        while (true)
        {
            ct.ThrowIfCancellationRequested();

            var token = await _credentials.GetIdTokenAsync(ct).ConfigureAwait(false);
            if (string.IsNullOrEmpty(token))
            {
                throw new ApiException(ApiErrorKind.Unauthorized, 401, null, "Not signed in.");
            }

            using var request = new HttpRequestMessage(
                method, _baseUrl is null ? new Uri(path, UriKind.Relative) : new Uri(_baseUrl(), path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            if (bodyFactory is not null)
            {
                request.Content = bodyFactory();
            }
            else if (jsonBody is not null)
            {
                request.Content = new StringContent(jsonBody, Encoding.UTF8, "application/json");
            }

            HttpResponseMessage response;
            try
            {
                response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            }
            catch (TaskCanceledException) when (!ct.IsCancellationRequested)
            {
                // HttpClient timeout — treat as retryable.
                if (attempt++ < retryLimit)
                {
                    await _delay(_retry.Backoff(attempt), ct).ConfigureAwait(false);
                    continue;
                }

                throw new ApiException(ApiErrorKind.Retryable, 408, null, "Request timed out.");
            }
            catch (HttpRequestException ex)
            {
                if (attempt++ < retryLimit)
                {
                    await _delay(_retry.Backoff(attempt), ct).ConfigureAwait(false);
                    continue;
                }

                throw new ApiException(ApiErrorKind.Retryable, null, null, $"Network error: {ex.Message}");
            }

            using (response)
            {
                var status = (int)response.StatusCode;
                if (response.IsSuccessStatusCode)
                {
                    var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
                    var value = await JsonSerializer.DeserializeAsync<T>(stream, cancellationToken: ct)
                        .ConfigureAwait(false);
                    return value ?? throw new ApiException(
                        ApiErrorKind.Unexpected, status, null, "Empty response body.");
                }

                var kind = ApiException.Classify(status);

                if (kind == ApiErrorKind.Unauthorized && !refreshedOnce)
                {
                    refreshedOnce = true;
                    var refreshed = await _credentials
                        .TryRefreshAfterUnauthorizedAsync(ct).ConfigureAwait(false);
                    if (refreshed)
                    {
                        continue; // retry once with the refreshed token
                    }
                }

                if (kind == ApiErrorKind.Retryable && attempt < retryLimit)
                {
                    attempt++;
                    await _delay(_retry.Backoff(attempt), ct).ConfigureAwait(false);
                    continue;
                }

                string? body = null;
                try
                {
                    body = await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
                }
                catch
                {
                    // ignore body read failures
                }

                throw ApiException.FromResponse(status, body);
            }
        }
    }
}
