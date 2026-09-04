using System.Net;
using System.Text.Json;

namespace VoicePrompt.Core.Api;

/// <summary>Coarse classification of an API failure, driving retry/auth behavior.</summary>
public enum ApiErrorKind
{
    /// <summary>400 — malformed request.</summary>
    BadRequest,

    /// <summary>401 — token invalid/expired/wrong audience. Refresh once, then require sign-in.</summary>
    Unauthorized,

    /// <summary>403 — valid token but the account is not allowlisted.</summary>
    Forbidden,

    /// <summary>404 — transcript not found or expired.</summary>
    NotFound,

    /// <summary>429 / 5xx / timeout / network — retry with backoff.</summary>
    Retryable,

    /// <summary>Any other non-success response.</summary>
    Unexpected,
}

/// <summary>A classified API error carrying the parsed <see cref="ProblemDetails"/> when present.</summary>
public sealed class ApiException : Exception
{
    public ApiErrorKind Kind { get; }
    public int? StatusCode { get; }
    public ProblemDetails? Problem { get; }

    public ApiException(ApiErrorKind kind, int? statusCode, ProblemDetails? problem, string message)
        : base(message)
    {
        Kind = kind;
        StatusCode = statusCode;
        Problem = problem;
    }

    /// <summary>Map an HTTP status code to an <see cref="ApiErrorKind"/>.</summary>
    public static ApiErrorKind Classify(int statusCode) => statusCode switch
    {
        400 => ApiErrorKind.BadRequest,
        401 => ApiErrorKind.Unauthorized,
        403 => ApiErrorKind.Forbidden,
        404 => ApiErrorKind.NotFound,
        408 => ApiErrorKind.Retryable,
        429 => ApiErrorKind.Retryable,
        >= 500 and <= 599 => ApiErrorKind.Retryable,
        _ => ApiErrorKind.Unexpected,
    };

    /// <summary>
    /// Build a classified exception from a status code and (possibly empty) response body,
    /// parsing an <c>application/problem+json</c> body when available. The response body is
    /// never propagated verbatim into logs — only structured fields.
    /// </summary>
    public static ApiException FromResponse(int statusCode, string? body)
    {
        var kind = Classify(statusCode);
        ProblemDetails? problem = null;
        if (!string.IsNullOrWhiteSpace(body))
        {
            try
            {
                problem = JsonSerializer.Deserialize<ProblemDetails>(body!);
            }
            catch (JsonException)
            {
                // Not a problem+json body; ignore.
            }
        }

        var title = problem?.Title ?? kind.ToString();
        return new ApiException(kind, statusCode, problem, $"API {statusCode}: {title}");
    }
}
