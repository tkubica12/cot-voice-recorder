using System.Collections.Concurrent;
using System.Net;
using System.Text;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;
using VoicePrompt.Core.Realtime;

namespace VoicePrompt.Core.Tests.Fakes;

/// <summary>Manually advanced clock.</summary>
public sealed class FakeClock : IClock
{
    public FakeClock(DateTimeOffset? start = null) =>
        UtcNow = start ?? new DateTimeOffset(2026, 9, 3, 12, 0, 0, TimeSpan.Zero);

    public DateTimeOffset UtcNow { get; set; }

    public void Advance(TimeSpan by) => UtcNow += by;
}

/// <summary>In-memory file system exercising the same atomic-write contract as the real one.</summary>
public sealed class FakeFileSystem : IFileSystem
{
    private readonly ConcurrentDictionary<string, byte[]> _files = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _dirs = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Temp files observed during atomic writes (must never be left behind).</summary>
    public List<string> ObservedTempFiles { get; } = new();

    public IReadOnlyCollection<string> Files => _files.Keys.ToList();

    public IReadOnlyCollection<string> Directories => _dirs.ToList();

    /// <summary>Set to make the next <see cref="AtomicWrite(string, byte[])"/> throw.</summary>
    public Exception? FailNextWrite { get; set; }

    public bool FileExists(string path) => _files.ContainsKey(path);

    public string ReadAllText(string path) => Encoding.UTF8.GetString(ReadAllBytes(path));

    public byte[] ReadAllBytes(string path) =>
        _files.TryGetValue(path, out var bytes) ? bytes : throw new FileNotFoundException(path);

    public void AtomicWrite(string path, byte[] bytes)
    {
        var temp = path + ".tmp";
        ObservedTempFiles.Add(temp);
        _files[temp] = bytes;

        if (FailNextWrite is not null)
        {
            var ex = FailNextWrite;
            FailNextWrite = null;
            _files.TryRemove(temp, out _);
            throw ex;
        }

        _files[path] = bytes;
        _files.TryRemove(temp, out _);
    }

    public void AtomicWrite(string path, string text) => AtomicWrite(path, Encoding.UTF8.GetBytes(text));

    public void Delete(string path) => _files.TryRemove(path, out _);

    public void CreateDirectory(string path) => _dirs.Add(path);

    public void Seed(string path, string text) => _files[path] = Encoding.UTF8.GetBytes(text);

    public void Seed(string path, byte[] bytes) => _files[path] = bytes;
}

/// <summary>
/// Reversible, non-DPAPI protector so <see cref="TokenStore"/> branches run on any platform.
/// Deliberately trivial (XOR + marker) — it is never used outside tests.
/// </summary>
public sealed class FakeSecretProtector : ISecretProtector
{
    private const byte Marker = 0xA5;
    private const byte Key = 0x5C;

    public bool FailUnprotect { get; set; }

    public int ProtectCalls { get; private set; }

    public byte[] Protect(byte[] plaintext)
    {
        ProtectCalls++;
        var result = new byte[plaintext.Length + 1];
        result[0] = Marker;
        for (var i = 0; i < plaintext.Length; i++)
        {
            result[i + 1] = (byte)(plaintext[i] ^ Key);
        }

        return result;
    }

    public byte[] Unprotect(byte[] ciphertext)
    {
        if (FailUnprotect || ciphertext.Length == 0 || ciphertext[0] != Marker)
        {
            throw new InvalidOperationException("cannot unprotect");
        }

        var result = new byte[ciphertext.Length - 1];
        for (var i = 0; i < result.Length; i++)
        {
            result[i] = (byte)(ciphertext[i + 1] ^ Key);
        }

        return result;
    }
}

/// <summary>Scripted <see cref="HttpMessageHandler"/> returning queued responses and recording requests.</summary>
public sealed class FakeHttpMessageHandler : HttpMessageHandler
{
    private readonly Queue<Func<HttpRequestMessage, HttpResponseMessage>> _responses = new();

    public List<HttpRequestMessage> Requests { get; } = new();

    /// <summary>
    /// Request bodies captured at send time. <see cref="HttpClient"/> disposes request content
    /// after the call, so assertions must read the buffered copy.
    /// </summary>
    public List<string> RequestBodies { get; } = new();

    public List<string?> AuthorizationValues { get; } = new();

    public FakeHttpMessageHandler Enqueue(Func<HttpRequestMessage, HttpResponseMessage> factory)
    {
        _responses.Enqueue(factory);
        return this;
    }

    public FakeHttpMessageHandler EnqueueJson(HttpStatusCode status, string json, string contentType = "application/json")
    {
        _responses.Enqueue(_ => new HttpResponseMessage(status)
        {
            Content = new StringContent(json, Encoding.UTF8, contentType),
        });
        return this;
    }

    public FakeHttpMessageHandler EnqueueProblem(HttpStatusCode status, string json) =>
        EnqueueJson(status, json, "application/problem+json");

    public FakeHttpMessageHandler EnqueueThrow(Exception exception)
    {
        _responses.Enqueue(_ => throw exception);
        return this;
    }

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
    {
        Requests.Add(request);
        AuthorizationValues.Add(request.Headers.Authorization?.Parameter);
        RequestBodies.Add(request.Content is null
            ? string.Empty
            : request.Content.ReadAsStringAsync(ct).GetAwaiter().GetResult());

        if (_responses.Count == 0)
        {
            throw new InvalidOperationException($"No scripted response for {request.Method} {request.RequestUri}");
        }

        return Task.FromResult(_responses.Dequeue()(request));
    }
}

/// <summary>Credentials that hand out a fixed token and count refresh attempts.</summary>
public sealed class FakeCredentials : IBackendCredentials
{
    public FakeCredentials(string? token = "token-1") => Token = token;

    public string? Token { get; set; }

    public string? TokenAfterRefresh { get; set; } = "token-2";

    public bool RefreshSucceeds { get; set; } = true;

    public int RefreshCalls { get; private set; }

    public List<string?> IssuedTokens { get; } = new();

    public Task<string?> GetIdTokenAsync(CancellationToken ct)
    {
        IssuedTokens.Add(Token);
        return Task.FromResult(Token);
    }

    public Task<bool> TryRefreshAfterUnauthorizedAsync(CancellationToken ct)
    {
        RefreshCalls++;
        if (RefreshSucceeds)
        {
            Token = TokenAfterRefresh;
        }

        return Task.FromResult(RefreshSucceeds);
    }
}

/// <summary>Clipboard writer that can be made to fail a set number of times.</summary>
public sealed class FakeClipboardWriter : IClipboardWriter
{
    public int FailuresRemaining { get; set; }

    public int Attempts { get; private set; }

    public string? LastText { get; private set; }

    public void SetText(string text)
    {
        Attempts++;
        if (FailuresRemaining > 0)
        {
            FailuresRemaining--;
            throw new InvalidOperationException("CLIPBRD_E_CANT_OPEN");
        }

        LastText = text;
    }
}

/// <summary>Records notifications so tests can assert what the user would have seen.</summary>
public sealed class RecordingNotifier : INotifier
{
    public List<(string Title, string Message, NotificationKind Kind)> Notifications { get; } = new();

    public void Notify(string title, string message, NotificationKind kind = NotificationKind.Info) =>
        Notifications.Add((title, message, kind));
}

/// <summary>Captures log lines so redaction can be asserted.</summary>
public sealed class RecordingLog : ILog
{
    public List<string> Lines { get; } = new();

    public void Write(LogLevel level, string message) => Lines.Add($"{level}: {message}");

    public string All => string.Join('\n', Lines);
}

/// <summary>Token client returning scripted responses for exchange and refresh.</summary>
public sealed class FakeGoogleTokenClient : IGoogleTokenClient
{
    public Queue<TokenResponse> ExchangeResponses { get; } = new();

    public Queue<TokenResponse> RefreshResponses { get; } = new();

    public Exception? RefreshThrows { get; set; }

    public int RefreshCalls { get; private set; }

    public int ExchangeCalls { get; private set; }

    public string? LastCodeVerifier { get; private set; }

    public string? LastRedirectUri { get; private set; }

    public Task<TokenResponse> ExchangeCodeAsync(
        DesktopClientConfig config, string code, string codeVerifier, string redirectUri, CancellationToken ct)
    {
        ExchangeCalls++;
        LastCodeVerifier = codeVerifier;
        LastRedirectUri = redirectUri;
        return Task.FromResult(ExchangeResponses.Count > 0 ? ExchangeResponses.Dequeue() : new TokenResponse());
    }

    public Task<TokenResponse> RefreshAsync(DesktopClientConfig config, string refreshToken, CancellationToken ct)
    {
        RefreshCalls++;
        if (RefreshThrows is not null)
        {
            throw RefreshThrows;
        }

        return Task.FromResult(RefreshResponses.Count > 0 ? RefreshResponses.Dequeue() : new TokenResponse());
    }
}

/// <summary>Browser that records the URL instead of launching anything.</summary>
public sealed class FakeBrowser : ISystemBrowser
{
    public List<string> OpenedUrls { get; } = new();

    public Task OpenAsync(string url, CancellationToken ct)
    {
        OpenedUrls.Add(url);
        return Task.CompletedTask;
    }
}

/// <summary>Loopback listener returning a scripted callback query.</summary>
public sealed class FakeLoopbackListener : ILoopbackAuthListener
{
    private readonly Func<string> _query;

    public FakeLoopbackListener(Func<string> query, string redirectUri = "http://127.0.0.1:5000/")
    {
        _query = query;
        RedirectUri = redirectUri;
    }

    public string RedirectUri { get; }

    public bool Disposed { get; private set; }

    public Task<string> WaitForCallbackAsync(CancellationToken ct) => Task.FromResult(_query());

    public void Dispose() => Disposed = true;
}

public sealed class FakeLoopbackListenerFactory : ILoopbackAuthListenerFactory
{
    private readonly Func<string> _query;

    public FakeLoopbackListenerFactory(Func<string> query) => _query = query;

    public List<FakeLoopbackListener> Created { get; } = new();

    public ILoopbackAuthListener Create()
    {
        var listener = new FakeLoopbackListener(_query);
        Created.Add(listener);
        return listener;
    }
}

/// <summary>Scripted realtime socket that replays frames then closes.</summary>
public sealed class FakeRealtimeSocket : IRealtimeSocket
{
    private readonly Queue<string> _frames;
    private readonly TaskCompletionSource<bool>? _holdOpen;

    public FakeRealtimeSocket(IEnumerable<string> frames, bool holdOpenAfterFrames = false)
    {
        _frames = new Queue<string>(frames);
        _holdOpen = holdOpenAfterFrames
            ? new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously)
            : null;
    }

    public Uri? ConnectedTo { get; private set; }

    public bool Closed { get; private set; }

    public bool Disposed { get; private set; }

    public Exception? ConnectThrows { get; set; }

    public Task ConnectAsync(Uri uri, CancellationToken ct)
    {
        if (ConnectThrows is not null)
        {
            throw ConnectThrows;
        }

        ConnectedTo = uri;
        return Task.CompletedTask;
    }

    public async Task<string?> ReceiveTextAsync(CancellationToken ct)
    {
        if (_frames.Count > 0)
        {
            return _frames.Dequeue();
        }

        if (_holdOpen is not null)
        {
            using var reg = ct.Register(() => _holdOpen.TrySetCanceled());
            await _holdOpen.Task.ConfigureAwait(false);
        }

        return null;
    }

    public Task CloseAsync(CancellationToken ct)
    {
        Closed = true;
        _holdOpen?.TrySetResult(true);
        return Task.CompletedTask;
    }

    public void Dispose() => Disposed = true;
}

/// <summary>Produces the queued sockets in order; extra connects get an immediately-closing socket.</summary>
public sealed class FakeRealtimeSocketFactory : IRealtimeSocketFactory
{
    private readonly Queue<FakeRealtimeSocket> _sockets;

    public FakeRealtimeSocketFactory(params FakeRealtimeSocket[] sockets) =>
        _sockets = new Queue<FakeRealtimeSocket>(sockets);

    public List<FakeRealtimeSocket> Created { get; } = new();

    public IRealtimeSocket Create()
    {
        var socket = _sockets.Count > 0
            ? _sockets.Dequeue()
            : new FakeRealtimeSocket(Array.Empty<string>(), holdOpenAfterFrames: true);
        Created.Add(socket);
        return socket;
    }
}

/// <summary>Negotiator returning scripted responses / failures.</summary>
public sealed class FakeNegotiator : IRealtimeNegotiator
{
    private readonly Queue<Func<NegotiateResponse>> _responses = new();

    public int Calls { get; private set; }

    public FakeNegotiator Enqueue(NegotiateResponse response)
    {
        _responses.Enqueue(() => response);
        return this;
    }

    public FakeNegotiator EnqueueThrow(Exception exception)
    {
        _responses.Enqueue(() => throw exception);
        return this;
    }

    public Task<NegotiateResponse> NegotiateAsync(CancellationToken ct)
    {
        Calls++;
        if (_responses.Count == 0)
        {
            throw new InvalidOperationException("no scripted negotiate response");
        }

        return Task.FromResult(_responses.Dequeue()());
    }
}
