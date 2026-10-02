using System.Globalization;
using System.Net.WebSockets;
using System.Text.Json;
using VoicePrompt.Core.Api;

namespace VoicePrompt.Core.Dictation;

public sealed record DictationStreamMessage(string Type, string ItemId = "", string Text = "", long ByteEnd = 0);

public interface IDictationStream : IAsyncDisposable
{
    Task SendAudioAsync(ReadOnlyMemory<byte> pcm, CancellationToken ct);
    Task CommitAsync(bool finish, CancellationToken ct);
    Task<DictationStreamMessage> ReceiveAsync(CancellationToken ct);
}

public interface IDictationStreamFactory
{
    Task<IDictationStream> ConnectAsync(string language, long startByte, CancellationToken ct);
}

public sealed class DictationStreamClient(Func<Uri> backend, IBackendCredentials credentials)
    : IDictationStreamFactory
{
    public async Task<IDictationStream> ConnectAsync(string language, long startByte, CancellationToken ct)
    {
        if (language is not ("auto" or "cs" or "en") || startByte < 0 || startByte % 2 != 0)
            throw new ArgumentException("Invalid streaming session configuration.");
        var endpoint = new UriBuilder(new Uri(backend(),
            "/v1/dictation/stream?language=" + Uri.EscapeDataString(language)
            + "&start_byte=" + startByte.ToString(CultureInfo.InvariantCulture)));
        if (endpoint.Scheme != Uri.UriSchemeHttps
            && !(endpoint.Scheme == Uri.UriSchemeHttp && endpoint.Uri.IsLoopback))
            throw new InvalidOperationException("Streaming requires HTTPS (or loopback HTTP for development).");
        endpoint.Scheme = endpoint.Scheme == Uri.UriSchemeHttps ? "wss" : "ws";
        for (var attempt = 0; attempt < 2; attempt++)
        {
            var token = await credentials.GetIdTokenAsync(ct).ConfigureAwait(false);
            if (string.IsNullOrWhiteSpace(token))
                throw new ApiException(ApiErrorKind.Unauthorized, 401, null, "Sign in before dictating.");
            var socket = new ClientWebSocket();
            var connection = new SocketStream(socket);
            try
            {
                socket.Options.SetRequestHeader("Authorization", "Bearer " + token);
                socket.Options.KeepAliveInterval = TimeSpan.FromSeconds(15);
                using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct);
                deadline.CancelAfter(TimeSpan.FromSeconds(25));
                await socket.ConnectAsync(endpoint.Uri, deadline.Token).ConfigureAwait(false);
                var ready = await connection.ReceiveReadyAsync(startByte, deadline.Token).ConfigureAwait(false);
                if (!ready) throw new InvalidDataException("Invalid streaming readiness response.");
                return connection;
            }
            catch (ApiException ex) when (ex.Kind == ApiErrorKind.Unauthorized && attempt == 0)
            {
                await connection.DisposeAsync();
                if (!await credentials.TryRefreshAfterUnauthorizedAsync(ct).ConfigureAwait(false)) throw;
            }
            catch
            {
                await connection.DisposeAsync();
                throw;
            }
        }
        throw new ApiException(ApiErrorKind.Unauthorized, 401, null, "Streaming authentication failed.");
    }

    private sealed class SocketStream(ClientWebSocket socket) : IDictationStream
    {
        public async Task SendAudioAsync(ReadOnlyMemory<byte> pcm, CancellationToken ct)
        {
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct);
            deadline.CancelAfter(TimeSpan.FromSeconds(25));
            try { await socket.SendAsync(pcm, WebSocketMessageType.Binary, true, deadline.Token).ConfigureAwait(false); }
            catch (OperationCanceledException) when (!ct.IsCancellationRequested)
            { throw new TimeoutException("Streaming audio sender stalled."); }
        }

        public async Task CommitAsync(bool finish, CancellationToken ct) =>
            await socket.SendAsync(finish ? "{\"type\":\"finish\"}"u8.ToArray() : "{\"type\":\"commit\"}"u8.ToArray(),
                WebSocketMessageType.Text, true, ct).ConfigureAwait(false);

        private async Task<JsonDocument> ReadAsync(CancellationToken ct)
        {
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct);
            deadline.CancelAfter(TimeSpan.FromSeconds(25));
            using var data = new MemoryStream();
            var buffer = new byte[4096];
            while (true)
            {
                var result = await socket.ReceiveAsync(buffer.AsMemory(), deadline.Token).ConfigureAwait(false);
                if (result.MessageType == WebSocketMessageType.Close)
                    throw new WebSocketException("Dictation stream closed before completion.");
                if (result.MessageType != WebSocketMessageType.Text || data.Length + result.Count > 2 * 1024 * 1024)
                    throw new InvalidDataException("Invalid or oversized streaming event.");
                data.Write(buffer, 0, result.Count);
                if (result.EndOfMessage) break;
            }
            return JsonDocument.Parse(data.ToArray());
        }

        private static void ThrowIfError(JsonElement root)
        {
            if (root.GetProperty("type").GetString() != "error") return;
            var code = root.TryGetProperty("code", out var value) ? value.GetString() : null;
            var status = code switch
            {
                "unauthorized" => 401, "forbidden" => 403, "invalid_request" => 422,
                "too_many_requests" => 429, _ => 503,
            };
            throw new ApiException(ApiException.Classify(status), status, null,
                "Streaming unavailable. Local audio is retained.");
        }

        internal async Task<bool> ReceiveReadyAsync(long expectedStart, CancellationToken ct)
        {
            using var document = await ReadAsync(ct).ConfigureAwait(false);
            var root = document.RootElement;
            ThrowIfError(root);
            return root.GetProperty("type").GetString() == "ready"
                && root.GetProperty("protocol").GetInt32() == 1
                && root.GetProperty("start_byte").GetInt64() == expectedStart;
        }

        public async Task<DictationStreamMessage> ReceiveAsync(CancellationToken ct)
        {
            using var document = await ReadAsync(ct).ConfigureAwait(false);
            var root = document.RootElement;
            ThrowIfError(root);
            var kind = root.GetProperty("type").GetString();
            if (kind is not ("delta" or "intermediate" or "confirmed" or "done"))
                throw new InvalidDataException("Unsupported streaming event.");
            var text = kind == "done" ? "" : root.GetProperty("text").GetString()
                ?? throw new InvalidDataException("Missing streaming text.");
            if (text.Length > 1_048_576) throw new InvalidDataException("Oversized streaming text.");
            var item = kind == "done" ? "" : root.GetProperty("item_id").GetString()
                ?? throw new InvalidDataException("Missing streaming item identity.");
            var end = kind is "confirmed" or "done" ? root.GetProperty("byte_end").GetInt64() : 0;
            if (end < 0 || end % 2 != 0) throw new InvalidDataException("Invalid confirmed audio position.");
            return new(kind, item, text, end);
        }

        public ValueTask DisposeAsync()
        {
            socket.Abort();
            socket.Dispose();
            return ValueTask.CompletedTask;
        }
    }
}
