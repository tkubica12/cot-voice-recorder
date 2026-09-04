using System.Diagnostics;
using System.Net;
using System.Text;

namespace VoicePrompt.Core.Auth;

/// <summary>Opens the URL with the user's default browser via the shell.</summary>
public sealed class SystemBrowser : ISystemBrowser
{
    public Task OpenAsync(string url, CancellationToken ct)
    {
        Process.Start(new ProcessStartInfo
        {
            FileName = url,
            UseShellExecute = true,
        })?.Dispose();
        return Task.CompletedTask;
    }
}

/// <summary>
/// An ephemeral <see cref="HttpListener"/> bound to <c>127.0.0.1</c> on an OS-assigned port.
/// It serves exactly one request — the OAuth redirect — replies with a small "you can close
/// this tab" page, and then stops. Binding to the loopback address (never <c>0.0.0.0</c>)
/// keeps the callback unreachable from the network.
/// </summary>
public sealed class LoopbackAuthListener : ILoopbackAuthListener
{
    private const string ResponseHtml =
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">" +
        "<title>VoicePrompt</title><style>body{font-family:Segoe UI,system-ui,sans-serif;" +
        "background:#f5f5f5;color:#111;display:flex;align-items:center;justify-content:center;" +
        "height:100vh;margin:0}div{text-align:center}h1{font-size:1.25rem;font-weight:600}" +
        "p{color:#6b7280}b{color:#f97316}</style></head><body><div>" +
        "<h1><b>VoicePrompt</b> sign-in complete</h1><p>You can close this tab and return to the app.</p>" +
        "</div></body></html>";

    private readonly HttpListener _listener = new();

    public LoopbackAuthListener()
    {
        var port = FindFreePort();
        RedirectUri = $"http://127.0.0.1:{port}/";
        _listener.Prefixes.Add(RedirectUri);
        _listener.Start();
    }

    public string RedirectUri { get; }

    public async Task<string> WaitForCallbackAsync(CancellationToken ct)
    {
        using var registration = ct.Register(Stop);

        HttpListenerContext context;
        try
        {
            context = await _listener.GetContextAsync().ConfigureAwait(false);
        }
        catch (Exception) when (ct.IsCancellationRequested)
        {
            throw new OperationCanceledException(ct);
        }
        catch (HttpListenerException ex)
        {
            throw new AuthCallbackException($"Loopback listener stopped before the callback arrived ({ex.ErrorCode}).");
        }

        var query = context.Request.Url?.Query ?? string.Empty;

        try
        {
            var bytes = Encoding.UTF8.GetBytes(ResponseHtml);
            context.Response.StatusCode = 200;
            context.Response.ContentType = "text/html; charset=utf-8";
            context.Response.ContentLength64 = bytes.Length;
            await context.Response.OutputStream.WriteAsync(bytes, ct).ConfigureAwait(false);
            context.Response.OutputStream.Close();
        }
        catch
        {
            // The browser may have already gone away; the code is what matters.
        }

        return query;
    }

    /// <summary>Ask the OS for an unused loopback TCP port.</summary>
    internal static int FindFreePort()
    {
        var probe = new System.Net.Sockets.TcpListener(IPAddress.Loopback, 0);
        probe.Start();
        try
        {
            return ((IPEndPoint)probe.LocalEndpoint).Port;
        }
        finally
        {
            probe.Stop();
        }
    }

    private void Stop()
    {
        try
        {
            if (_listener.IsListening)
            {
                _listener.Stop();
            }
        }
        catch
        {
            // best effort
        }
    }

    public void Dispose()
    {
        Stop();
        _listener.Close();
    }
}

/// <summary>Creates a fresh listener (and therefore a fresh port) per sign-in attempt.</summary>
public sealed class LoopbackAuthListenerFactory : ILoopbackAuthListenerFactory
{
    public static readonly LoopbackAuthListenerFactory Instance = new();

    public ILoopbackAuthListener Create() => new LoopbackAuthListener();
}
