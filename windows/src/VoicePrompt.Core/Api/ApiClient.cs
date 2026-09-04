using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using VoicePrompt.Core.Infrastructure;

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
    private readonly HttpClient _http;
    private readonly IBackendCredentials _credentials;
    private readonly ApiRetryOptions _retry;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;

    public ApiClient(
        HttpClient http,
        IBackendCredentials credentials,
        ApiRetryOptions? retry = null,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        _http = http;
        _credentials = credentials;
        _retry = retry ?? new ApiRetryOptions();
        _delay = delay ?? Task.Delay;
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

    private async Task<T> SendAsync<T>(HttpMethod method, string path, string? jsonBody, CancellationToken ct)
    {
        var refreshedOnce = false;
        var attempt = 0;

        while (true)
        {
            ct.ThrowIfCancellationRequested();

            var token = await _credentials.GetIdTokenAsync(ct).ConfigureAwait(false);
            if (string.IsNullOrEmpty(token))
            {
                throw new ApiException(ApiErrorKind.Unauthorized, 401, null, "Not signed in.");
            }

            using var request = new HttpRequestMessage(method, path);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            if (jsonBody is not null)
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
                if (attempt++ < _retry.MaxRetries)
                {
                    await _delay(_retry.Backoff(attempt), ct).ConfigureAwait(false);
                    continue;
                }

                throw new ApiException(ApiErrorKind.Retryable, 408, null, "Request timed out.");
            }
            catch (HttpRequestException ex)
            {
                if (attempt++ < _retry.MaxRetries)
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

                if (kind == ApiErrorKind.Retryable && attempt < _retry.MaxRetries)
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
