using VoicePrompt.Core.Api;
using VoicePrompt.Core.Infrastructure;

namespace VoicePrompt.Core.Realtime;

/// <summary>Connection state surfaced to the UI.</summary>
public enum RealtimeState
{
    /// <summary>Not started (or stopped).</summary>
    Stopped,

    /// <summary>Explicitly suspended (machine suspend / user pause of the connection).</summary>
    Suspended,

    /// <summary>Negotiating / opening the WebSocket.</summary>
    Connecting,

    /// <summary>Connected and receiving.</summary>
    Connected,

    /// <summary>Disconnected and waiting out the backoff before the next attempt.</summary>
    Reconnecting,

    /// <summary>Negotiate was rejected (401 after refresh, or 403): interactive sign-in required.</summary>
    SignInRequired,

    /// <summary>No Desktop OAuth client is configured; realtime is intentionally idle.</summary>
    NotConfigured,
}

/// <summary>Abstracts <c>POST /v1/realtime/negotiate</c> so the realtime loop is testable.</summary>
public interface IRealtimeNegotiator
{
    Task<NegotiateResponse> NegotiateAsync(CancellationToken ct);
}

/// <summary>Negotiator backed by the real typed <see cref="ApiClient"/>.</summary>
public sealed class ApiRealtimeNegotiator : IRealtimeNegotiator
{
    private readonly ApiClient _api;
    private readonly string _appVersion;

    public ApiRealtimeNegotiator(ApiClient api, string appVersion)
    {
        _api = api;
        _appVersion = appVersion;
    }

    public Task<NegotiateResponse> NegotiateAsync(CancellationToken ct) =>
        _api.NegotiateAsync("windows", _appVersion, ct);
}

/// <summary>
/// Maintains a direct Azure Web PubSub connection: authenticated negotiate, WebSocket connect
/// with the <c>json.webpubsub.azure.v1</c> subprotocol, envelope parsing, jittered bounded
/// reconnect, proactive access-URL renewal before expiry, and suspend/resume. The backend is
/// never polled — the only backend calls are negotiate and the on-event transcript fetch.
/// </summary>
public sealed class RealtimeClient : IAsyncDisposable
{
    private readonly IRealtimeNegotiator _negotiator;
    private readonly IRealtimeSocketFactory _sockets;
    private readonly IClock _clock;
    private readonly ILog _log;
    private readonly ReconnectPolicy _policy;
    private readonly TimeSpan _renewSkew;
    private readonly TimeSpan _signInRetryDelay;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;
    private readonly object _gate = new();

    private CancellationTokenSource? _lifetime;
    private CancellationTokenSource? _connection;
    private CancellationTokenSource? _retryWait;
    private TaskCompletionSource<bool> _resume = Completed();
    private Task? _loop;
    private RealtimeState _state = RealtimeState.Stopped;
    private volatile bool _suspended;

    public RealtimeClient(
        IRealtimeNegotiator negotiator,
        IRealtimeSocketFactory sockets,
        IClock clock,
        ILog? log = null,
        ReconnectPolicy? policy = null,
        TimeSpan? renewSkew = null,
        TimeSpan? signInRetryDelay = null,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        _negotiator = negotiator;
        _sockets = sockets;
        _clock = clock;
        _log = log ?? NullLog.Instance;
        _policy = policy ?? ReconnectPolicy.Default;
        _renewSkew = renewSkew ?? TimeSpan.FromMinutes(2);
        _signInRetryDelay = signInRetryDelay ?? TimeSpan.FromMinutes(5);
        _delay = delay ?? Task.Delay;
    }

    /// <summary>Raised on every connection-state transition.</summary>
    public event Action<RealtimeState>? StateChanged;

    /// <summary>Raised for each received <c>transcript.completed</c> event (not yet deduped).</summary>
    public event Func<TranscriptCompletedEvent, CancellationToken, Task>? TranscriptCompleted;

    public RealtimeState State
    {
        get { lock (_gate) { return _state; } }
    }

    public bool IsRunning => _loop is { IsCompleted: false };

    /// <summary>Start the connection loop. Idempotent.</summary>
    public void Start()
    {
        lock (_gate)
        {
            if (_loop is { IsCompleted: false })
            {
                return;
            }

            _lifetime = new CancellationTokenSource();
            _loop = Task.Run(() => RunAsync(_lifetime.Token));
        }
    }

    /// <summary>Stop the loop and close the socket.</summary>
    public async Task StopAsync()
    {
        Task? loop;
        lock (_gate)
        {
            loop = _loop;
            _lifetime?.Cancel();
            _connection?.Cancel();
            _retryWait?.Cancel();
            _resume.TrySetResult(true);
        }

        if (loop is not null)
        {
            try
            {
                await loop.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // expected
            }
        }

        lock (_gate)
        {
            _loop = null;
            _lifetime?.Dispose();
            _lifetime = null;
        }

        SetState(RealtimeState.Stopped);
    }

    /// <summary>Drop the connection and idle (machine suspend / sleep).</summary>
    public void Suspend()
    {
        lock (_gate)
        {
            if (_suspended)
            {
                return;
            }

            _suspended = true;
            _resume = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            _connection?.Cancel();
            _retryWait?.Cancel();
        }

        _log.Info("realtime: suspended");
        SetState(RealtimeState.Suspended);
    }

    /// <summary>Resume after a suspend and reconnect immediately.</summary>
    public void Resume()
    {
        lock (_gate)
        {
            if (!_suspended)
            {
                return;
            }

            _suspended = false;
            _resume.TrySetResult(true);
        }

        _log.Info("realtime: resumed");
    }

    /// <summary>Force an immediate renegotiate + reconnect (e.g. after an interactive sign-in).</summary>
    public void Reconnect()
    {
        lock (_gate)
        {
            _connection?.Cancel();
            _retryWait?.Cancel();
        }
    }

    private async Task RunAsync(CancellationToken lifetime)
    {
        var attempt = 0;

        while (!lifetime.IsCancellationRequested)
        {
            if (_suspended)
            {
                SetState(RealtimeState.Suspended);
                try
                {
                    await WaitForResumeAsync(lifetime).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }

                attempt = 0;
                continue;
            }

            var reconnectDelay = TimeSpan.Zero;

            using var connection = CancellationTokenSource.CreateLinkedTokenSource(lifetime);
            lock (_gate) { _connection = connection; }

            IRealtimeSocket? socket = null;
            try
            {
                SetState(RealtimeState.Connecting);
                var access = await _negotiator.NegotiateAsync(connection.Token).ConfigureAwait(false);

                if (string.IsNullOrWhiteSpace(access.Url)
                    || !Uri.TryCreate(access.Url, UriKind.Absolute, out var uri))
                {
                    throw new InvalidOperationException("negotiate returned no usable client URL");
                }

                // Renew ahead of expiry: cancelling the receive forces a fresh negotiate.
                var lifetimeLeft = access.ExpiresAt - _clock.UtcNow - _renewSkew;
                if (lifetimeLeft > TimeSpan.Zero && lifetimeLeft < TimeSpan.FromDays(1))
                {
                    connection.CancelAfter(lifetimeLeft);
                }

                socket = _sockets.Create();
                await socket.ConnectAsync(uri, connection.Token).ConfigureAwait(false);

                attempt = 0;
                SetState(RealtimeState.Connected);
                _log.Info($"realtime: connected (hub={Sanitize(access.Hub)}, group={Sanitize(access.Group)})");

                await ReceiveLoopAsync(socket, connection.Token).ConfigureAwait(false);
                _log.Info("realtime: connection closed by peer or renewal");
            }
            catch (OperationCanceledException) when (lifetime.IsCancellationRequested)
            {
                break;
            }
            catch (OperationCanceledException)
            {
                // Renewal deadline, Suspend(), or Reconnect(): loop again immediately.
                reconnectDelay = TimeSpan.Zero;
                attempt = 0;
            }
            catch (ApiException ex) when (ex.Kind is ApiErrorKind.Unauthorized or ApiErrorKind.Forbidden)
            {
                SetState(RealtimeState.SignInRequired);
                _log.Warn($"realtime: negotiate rejected ({ex.StatusCode}); sign-in required");
                reconnectDelay = _signInRetryDelay;
            }
            catch (Exception ex)
            {
                attempt++;
                reconnectDelay = _policy.NextDelay(attempt);
                _log.Warn($"realtime: attempt {attempt} failed ({ex.GetType().Name}); retrying in {reconnectDelay.TotalSeconds:F1}s");
            }
            finally
            {
                lock (_gate) { _connection = null; }
                if (socket is not null)
                {
                    try
                    {
                        await socket.CloseAsync(CancellationToken.None).ConfigureAwait(false);
                    }
                    catch
                    {
                        // best effort
                    }

                    socket.Dispose();
                }
            }

            if (lifetime.IsCancellationRequested)
            {
                break;
            }

            if (reconnectDelay > TimeSpan.Zero)
            {
                SetState(State == RealtimeState.SignInRequired
                    ? RealtimeState.SignInRequired
                    : RealtimeState.Reconnecting);
                using var retryWait = CancellationTokenSource.CreateLinkedTokenSource(lifetime);
                lock (_gate) { _retryWait = retryWait; }
                try
                {
                    await _delay(reconnectDelay, retryWait.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException) when (lifetime.IsCancellationRequested)
                {
                    break;
                }
                catch (OperationCanceledException)
                {
                    // Reconnect() interrupts auth/backoff waits so fresh credentials are used now.
                    attempt = 0;
                }
                finally
                {
                    lock (_gate)
                    {
                        if (ReferenceEquals(_retryWait, retryWait))
                        {
                            _retryWait = null;
                        }
                    }
                }
            }
        }

        SetState(RealtimeState.Stopped);
    }

    private async Task ReceiveLoopAsync(IRealtimeSocket socket, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            var text = await socket.ReceiveTextAsync(ct).ConfigureAwait(false);
            if (text is null)
            {
                return;
            }

            var frame = WebPubSubEnvelope.Parse(text);
            if (frame.IsDisconnected)
            {
                _log.Info("realtime: service sent disconnected");
                return;
            }

            if (!WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out var completed))
            {
                continue;
            }

            var handler = TranscriptCompleted;
            if (handler is null)
            {
                continue;
            }

            try
            {
                await handler(completed, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                throw;
            }
            catch (Exception ex)
            {
                // A failed handler must not kill the connection.
                _log.Error($"realtime: event handler failed ({ex.GetType().Name})");
            }
        }
    }

    private async Task WaitForResumeAsync(CancellationToken ct)
    {
        TaskCompletionSource<bool> source;
        lock (_gate) { source = _resume; }

        using var registration = ct.Register(static s => ((TaskCompletionSource<bool>)s!).TrySetCanceled(), source);
        await source.Task.ConfigureAwait(false);
    }

    private void SetState(RealtimeState state)
    {
        bool changed;
        lock (_gate)
        {
            changed = _state != state;
            _state = state;
        }

        if (changed)
        {
            StateChanged?.Invoke(state);
        }
    }

    private static string Sanitize(string? value) =>
        string.IsNullOrEmpty(value) ? "-" : new string(value.Where(char.IsLetterOrDigit).ToArray());

    private static TaskCompletionSource<bool> Completed()
    {
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        tcs.SetResult(true);
        return tcs;
    }

    public async ValueTask DisposeAsync() => await StopAsync().ConfigureAwait(false);
}
