using System.Net.WebSockets;
using System.Text;

namespace VoicePrompt.Core.Realtime;

/// <summary>
/// Minimal WebSocket seam so the realtime client can be driven by a fake in tests. Mirrors
/// the subset of <see cref="ClientWebSocket"/> the app uses: connect, receive text, close.
/// </summary>
public interface IRealtimeSocket : IDisposable
{
    Task ConnectAsync(Uri uri, CancellationToken ct);

    /// <summary>Receive the next complete text frame, or <c>null</c> when the peer closed.</summary>
    Task<string?> ReceiveTextAsync(CancellationToken ct);

    Task CloseAsync(CancellationToken ct);
}

/// <summary>Creates a fresh socket per connection attempt (a <see cref="ClientWebSocket"/> is single-use).</summary>
public interface IRealtimeSocketFactory
{
    IRealtimeSocket Create();
}

/// <summary>
/// <see cref="ClientWebSocket"/>-backed transport negotiating the
/// <c>json.webpubsub.azure.v1</c> subprotocol so the service wraps payloads in the documented
/// server envelope.
/// </summary>
public sealed class ClientWebSocketTransport : IRealtimeSocket
{
    public const string SubProtocol = "json.webpubsub.azure.v1";

    private const int ReceiveBufferSize = 8 * 1024;
    private const int MaxFrameBytes = 1024 * 1024;

    private readonly ClientWebSocket _socket = new();

    public ClientWebSocketTransport()
    {
        _socket.Options.AddSubProtocol(SubProtocol);
        _socket.Options.KeepAliveInterval = TimeSpan.FromSeconds(30);
    }

    public Task ConnectAsync(Uri uri, CancellationToken ct) => _socket.ConnectAsync(uri, ct);

    public async Task<string?> ReceiveTextAsync(CancellationToken ct)
    {
        var buffer = new byte[ReceiveBufferSize];
        using var accumulated = new MemoryStream();

        while (true)
        {
            WebSocketReceiveResult result;
            try
            {
                result = await _socket.ReceiveAsync(new ArraySegment<byte>(buffer), ct).ConfigureAwait(false);
            }
            catch (WebSocketException)
            {
                return null;
            }
            catch (ObjectDisposedException)
            {
                return null;
            }

            if (result.MessageType == WebSocketMessageType.Close)
            {
                return null;
            }

            accumulated.Write(buffer, 0, result.Count);
            if (accumulated.Length > MaxFrameBytes)
            {
                // Defensive bound: the service never sends frames this large.
                return null;
            }

            if (result.EndOfMessage)
            {
                return result.MessageType == WebSocketMessageType.Binary
                    ? string.Empty
                    : Encoding.UTF8.GetString(accumulated.GetBuffer(), 0, (int)accumulated.Length);
            }
        }
    }

    public async Task CloseAsync(CancellationToken ct)
    {
        try
        {
            if (_socket.State == WebSocketState.Open)
            {
                await _socket.CloseAsync(WebSocketCloseStatus.NormalClosure, "bye", ct).ConfigureAwait(false);
            }
        }
        catch
        {
            // Closing is best-effort.
        }
    }

    public void Dispose() => _socket.Dispose();
}

/// <summary>Default factory producing real <see cref="ClientWebSocketTransport"/> instances.</summary>
public sealed class ClientWebSocketTransportFactory : IRealtimeSocketFactory
{
    public static readonly ClientWebSocketTransportFactory Instance = new();

    public IRealtimeSocket Create() => new ClientWebSocketTransport();
}
