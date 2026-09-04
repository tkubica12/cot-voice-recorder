namespace VoicePrompt.Core.Api;

/// <summary>
/// Supplies backend bearer credentials (Google <b>ID tokens</b>) and a single-shot refresh on
/// 401. Implemented by the auth layer; abstracted so the API client is testable.
/// </summary>
public interface IBackendCredentials
{
    /// <summary>Current valid ID token, or <c>null</c> when sign-in is required.</summary>
    Task<string?> GetIdTokenAsync(CancellationToken ct);

    /// <summary>
    /// Attempt exactly one refresh after a 401. Returns <c>true</c> if a (possibly) fresh token
    /// is now available to retry with; <c>false</c> if the caller should surface sign-in-needed.
    /// </summary>
    Task<bool> TryRefreshAfterUnauthorizedAsync(CancellationToken ct);
}
