using System.Collections.Concurrent;
using System.Net;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Dictation;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public sealed class RollingDictationIntegrationTests
{
    [Fact]
    public async Task Twenty_minutes_of_audio_sized_arrivals_keep_order_and_bounded_background_requests()
    {
        var clock = new VirtualTime();
        using var handler = new PatchHandler();
        using var http = new HttpClient(handler) { BaseAddress = new("https://example.test") };
        var api = new ApiClient(http, new Credentials());
        await using var polisher = new DictationPolisher(api.RefineDictationAsync, timeProvider: clock);
        var raw = new StringBuilder();
        for (var chunk = 0; chunk < 200; chunk++)
        {
            raw.Append($"segment{chunk:000} again again this is a synthetic audio chunk with ordered content. ");
            clock.Advance(TimeSpan.FromSeconds(6));
            polisher.Update(raw.ToString());
            await UntilAsync(() => polisher.Progress.PendingBlocks == 0);
        }
        polisher.StopScheduling();
        var requestsAtStop = handler.Requests.Count;
        const string tail = "The final tail must never be lost.";
        var completion = polisher.CompleteAsync(raw + tail, CancellationToken.None);
        Assert.True(completion.IsCompleted, "Final assembly must not await the model.");
        var result = await completion;
        Assert.Equal(raw + tail, result.RawText);
        Assert.EndsWith(tail, result.Text);
        Assert.Equal(0, result.FallbackBlocks);
        Assert.Contains("segment000 again this", result.Text);
        var markers = Regex.Matches(result.Text, @"segment\d{3}").Select(match => match.Value).ToArray();
        Assert.Equal(Enumerable.Range(0, 200).Select(index => $"segment{index:000}"), markers);
        Assert.InRange(handler.Requests.Count, 1, 199);
        Assert.All(handler.Requests, request =>
        {
            Assert.InRange(request.Current.Length, 1, 4000);
            Assert.InRange(request.Previous.Length, 0, 8000);
        });
        await Task.Delay(30);
        Assert.Equal(requestsAtStop, handler.Requests.Count);
        Assert.Equal(result.Text, polisher.Progress.Text);
    }

    private static async Task UntilAsync(Func<bool> condition)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (!condition()) await Task.Delay(1, timeout.Token);
    }

    private sealed class VirtualTime : TimeProvider
    {
        private long _ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
        public override long GetTimestamp() => _ticks;
        public override DateTimeOffset GetUtcNow() => DateTimeOffset.UnixEpoch.AddTicks(_ticks);
        public void Advance(TimeSpan elapsed) => _ticks += elapsed.Ticks;
    }

    private sealed class PatchHandler : HttpMessageHandler
    {
        public ConcurrentQueue<(string Current, string Previous)> Requests { get; } = new();

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct));
            var current = body.RootElement.GetProperty("text").GetString()!;
            var previous = body.RootElement.GetProperty("previous_text").GetString()!;
            Requests.Enqueue((current, previous));
            var edits = Regex.Matches(current, @"segment\d{3} again again")
                .Select(match => new { original = match.Value, replacement = match.Value[..^6] }).ToArray();
            var text = current;
            foreach (var edit in edits) text = text.Replace(edit.original, edit.replacement, StringComparison.Ordinal);
            return new(HttpStatusCode.OK)
            {
                Content = new StringContent(JsonSerializer.Serialize(new { text, edits }), Encoding.UTF8, "application/json"),
            };
        }
    }

    private sealed class Credentials : IBackendCredentials
    {
        public Task<string?> GetIdTokenAsync(CancellationToken ct) => Task.FromResult<string?>("synthetic-token");
        public Task<bool> TryRefreshAfterUnauthorizedAsync(CancellationToken ct) => Task.FromResult(false);
    }
}
