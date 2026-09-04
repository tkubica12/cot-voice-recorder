namespace VoicePrompt.Core.Auth;

/// <summary>Opens a URL in the user's default system browser (never an embedded browser).</summary>
public interface ISystemBrowser
{
    Task OpenAsync(string url, CancellationToken ct);
}

/// <summary>
/// An ephemeral loopback HTTP listener bound to 127.0.0.1 that captures the single OAuth
/// redirect callback. <see cref="RedirectUri"/> is the exact URI to pass to Google.
/// </summary>
public interface ILoopbackAuthListener : IDisposable
{
    /// <summary>The <c>http://127.0.0.1:{port}/</c> redirect URI this listener is bound to.</summary>
    string RedirectUri { get; }

    /// <summary>Await the browser redirect and return its raw query string (including leading '?').</summary>
    Task<string> WaitForCallbackAsync(CancellationToken ct);
}

/// <summary>Factory so a fresh loopback listener (new ephemeral port) is created per sign-in.</summary>
public interface ILoopbackAuthListenerFactory
{
    ILoopbackAuthListener Create();
}
