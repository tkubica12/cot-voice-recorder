using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Realtime;
using VoicePrompt.Core.Settings;

try
{
    using var input = new MemoryStream();
    Console.OpenStandardInput().CopyTo(input);
    var pcm = input.ToArray();
    using var output = new BinaryWriter(Console.OpenStandardOutput());
    if (args.Length == 3 && args[0] is "recovery-create" or "recovery-inspect")
    {
        if (!OperatingSystem.IsWindows())
            throw new PlatformNotSupportedException("Recovery parity requires Windows.");
        var store = new RecoveryStore(args[1], new DpapiSecretProtector());
        using var journal = args[0] == "recovery-inspect"
            ? store.Open(args[2])
            : store.Create("auto", "synthetic context", streaming: args[2] != "legacy");
        if (args[0] == "recovery-create")
        {
            var audio = PcmChunker.ToWav(Enumerable.Repeat((byte)1, 6400).ToArray());
            journal.Checkpoint([], 12800, [
                new AudioChunk(audio, false, 6400), new AudioChunk(audio, false, 12800)
            ], complete: true);
            if (args[2] == "legacy") journal.SaveResult(0, "synthetic first");
            else
            {
                journal.SaveStreamingResult(6400, "synthetic first", new byte[100]);
                if (args[2] == "audit")
                {
                    journal.SaveStreamingResult(12800, "synthetic first final", new byte[100]);
                    journal.PrepareAudit("synthetic polished final", SystemClock.Instance.UtcNow, true);
                }
            }
        }
        output.Write(JsonSerializer.SerializeToUtf8Bytes(journal.Snapshot));
    }
    else if (args.Length == 2 && args[0] is "settings-save" or "settings-load")
    {
        var store = new SettingsStore(args[1], PhysicalFileSystem.Instance);
        if (args[0] == "settings-save")
            store.Save(JsonSerializer.Deserialize<AppSettings>(pcm)
                ?? throw new InvalidDataException("Invalid settings fixture."));
        else
            output.Write(JsonSerializer.SerializeToUtf8Bytes(store.Load()));
    }
    else if (args.Length == 2 && args[0] is "history-add" or "history-load")
    {
        var store = new HistoryStore(args[1], PhysicalFileSystem.Instance, SystemClock.Instance);
        if (args[0] == "history-add")
        {
            var entries = JsonSerializer.Deserialize<List<HistoryEntry>>(pcm)
                ?? throw new InvalidDataException("Invalid history fixture.");
            if (entries.Count != 1) throw new InvalidDataException("Expected one history fixture.");
            store.Add(entries[0]);
        }
        else
        {
            output.Write(JsonSerializer.SerializeToUtf8Bytes(store.All(),
                new JsonSerializerOptions { DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull }));
        }
    }
    else if (args.Length == 2 && args[0] is "cache-save" or "cache-load" or "cache-clear")
    {
        if (!OperatingSystem.IsWindows())
            throw new PlatformNotSupportedException("Token-store reference requires Windows.");
        var store = new TokenStore(args[1], new DpapiSecretProtector(), PhysicalFileSystem.Instance);
        switch (args[0])
        {
            case "cache-save":
                store.Save(JsonSerializer.Deserialize<CachedTokens>(pcm)
                    ?? throw new InvalidDataException("Invalid cached-token fixture."));
                break;
            case "cache-load":
                output.Write(JsonSerializer.SerializeToUtf8Bytes(store.Load(),
                    new JsonSerializerOptions { DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull }));
                break;
            case "cache-clear": store.Clear(); break;
        }
    }
    else if (args.Length == 1 && args[0] is "cache" or "pkce" or "callback" or "auth-url" or "jwt" or "refresh" or "frame"
        or "normalize-dictation" or "normalize-batch" or "stitch" or "append-prefix" or "legacy-state")
    {
        var text = Encoding.UTF8.GetString(pcm);
        byte[] JsonBytes(object? value) => JsonSerializer.SerializeToUtf8Bytes(value);
        switch (args[0])
        {
            case "normalize-dictation":
                var normalizer = typeof(DictationSession).GetMethod("Normalize",
                    System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic)
                    ?? throw new InvalidDataException("Missing reference normalizer.");
                var normalized = normalizer.Invoke(null, [text]) as string
                    ?? throw new InvalidDataException("Invalid reference normalizer output.");
                output.Write(Encoding.UTF8.GetBytes(normalized));
                break;
            case "normalize-batch":
                using (var document = JsonDocument.Parse(pcm))
                {
                    var method = typeof(DictationSession).GetMethod("Normalize",
                        System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic)
                        ?? throw new InvalidDataException("Missing reference normalizer.");
                    var results = document.RootElement.EnumerateArray().Select(item =>
                        method.Invoke(null, [item.GetString()]) as string
                            ?? throw new InvalidDataException("Invalid reference normalizer output.")).ToArray();
                    output.Write(JsonSerializer.SerializeToUtf8Bytes(results));
                }
                break;
            case "stitch":
                using (var document = JsonDocument.Parse(pcm))
                    output.Write(Encoding.UTF8.GetBytes(DictationSession.Stitch(
                        document.RootElement.EnumerateArray().Select(item => (
                            item.GetProperty("text").GetString()
                                ?? throw new InvalidDataException("Missing fixture text."),
                            item.GetProperty("overlap").GetBoolean())))));
                break;
            case "append-prefix":
                using (var document = JsonDocument.Parse(pcm))
                {
                    var root = document.RootElement;
                    var append = typeof(DictationSession).GetMethod("AppendPreservingPrefix",
                        System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic)
                        ?? throw new InvalidDataException("Missing reference append method.");
                    int? previousWords = root.TryGetProperty("previous_words", out var value)
                        && value.ValueKind != JsonValueKind.Null ? value.GetInt32() : null;
                    var appended = append.Invoke(null, [
                        root.GetProperty("previous").GetString(), root.GetProperty("incoming").GetString(),
                        root.GetProperty("overlap").GetBoolean(), previousWords
                    ]) as string ?? throw new InvalidDataException("Invalid reference append output.");
                    output.Write(Encoding.UTF8.GetBytes(appended));
                }
                break;
            case "legacy-state":
                using (var chunker = new PcmChunker())
                {
                    chunker.Append(pcm);
                    output.Write(JsonSerializer.SerializeToUtf8Bytes(chunker.Snapshot()));
                }
                break;
            case "cache":
                var tokens = JsonSerializer.Deserialize<CachedTokens>(pcm)
                    ?? throw new InvalidDataException("Invalid cached-token fixture.");
                output.Write(JsonSerializer.SerializeToUtf8Bytes(tokens,
                    new JsonSerializerOptions { DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull }));
                break;
            case "pkce": output.Write(Encoding.ASCII.GetBytes(PkceGenerator.Challenge(text))); break;
            case "callback":
                using (var document = JsonDocument.Parse(pcm))
                    output.Write(Encoding.UTF8.GetBytes(AuthorizationRequestBuilder.ParseAndValidateCallback(
                        document.RootElement.GetProperty("query").GetString()!,
                        document.RootElement.GetProperty("state").GetString()!)));
                break;
            case "auth-url":
                using (var document = JsonDocument.Parse(pcm))
                {
                    var root = document.RootElement;
                    var verifier = root.GetProperty("verifier").GetString()!;
                    var pair = new PkcePair(verifier, PkceGenerator.Challenge(verifier), root.GetProperty("state").GetString()!);
                    var url = AuthorizationRequestBuilder.BuildAuthorizationUrl(
                        DesktopClientConfig.Parse(root.GetProperty("config").GetRawText()),
                        root.GetProperty("redirect").GetString()!, pair,
                        loginHint: root.TryGetProperty("login_hint", out var hint) ? hint.GetString() : null);
                    output.Write(Encoding.UTF8.GetBytes(url));
                }
                break;
            case "jwt":
                var jwt = JwtToken.Parse(text);
                output.Write(JsonBytes(new { exp = jwt.ExpiresAt?.ToUnixTimeSeconds(), email = jwt.Email, aud = jwt.Audience, sub = jwt.Subject }));
                break;
            case "refresh":
                output.Write(Encoding.ASCII.GetBytes(AuthManager.ClassifyRefresh(
                    JsonSerializer.Deserialize<TokenResponse>(pcm)!).ToString()));
                break;
            case "frame":
                var frame = WebPubSubEnvelope.Parse(text);
                var valid = WebPubSubEnvelope.TryReadTranscriptCompleted(frame, out var completed);
                output.Write(JsonBytes(new {
                    kind = frame.Kind.ToString(), system_event = frame.SystemEvent,
                    from = frame.From, group = frame.Group, data_type = frame.DataType,
                    user_id = frame.UserId, connection_id = frame.ConnectionId,
                    completed = valid ? new {
                        event_id = completed.EventId, transcript_id = completed.TranscriptId,
                        recording_id = completed.RecordingId, preview = completed.Preview,
                    } : null
                }));
                break;
        }
    }
    else if (args.Length == 1 && args[0] is "protect" or "unprotect")
    {
        if (!OperatingSystem.IsWindows())
            throw new PlatformNotSupportedException("DPAPI reference requires Windows.");
        var protector = new DpapiSecretProtector();
        output.Write(args[0] == "protect" ? protector.Protect(pcm) : protector.Unprotect(pcm));
    }
    else if (args.Length == 1 && args[0] == "wrap")
    {
        output.Write(PcmChunker.ToWav(pcm));
    }
    else if (args.Length is 2 or 3 && args[0] == "chunk")
    {
        var fragment = int.Parse(args[1], CultureInfo.InvariantCulture);
        var boundary = args.Length == 3 ? int.Parse(args[2], CultureInfo.InvariantCulture) : pcm.Length;
        if (fragment <= 0 || boundary < 0 || boundary > pcm.Length)
            throw new ArgumentException("Invalid fragment size or restore boundary.");
        var chunker = new PcmChunker();
        try
        {
            var chunks = new List<AudioChunk>();
            for (var offset = 0; offset < boundary; offset += fragment)
                chunks.AddRange(chunker.Append(pcm.AsSpan(offset, Math.Min(fragment, boundary - offset))));
            if (args.Length == 3)
            {
                var restored = new PcmChunker(chunker.Snapshot());
                chunker.Dispose();
                chunker = restored;
                for (var offset = boundary; offset < pcm.Length; offset += fragment)
                    chunks.AddRange(chunker.Append(pcm.AsSpan(offset, Math.Min(fragment, pcm.Length - offset))));
            }
            chunks.AddRange(chunker.Flush());
            output.Write(chunks.Count);
            foreach (var chunk in chunks)
            {
                output.Write(chunk.EndByte);
                output.Write(chunk.OverlapsPrevious);
                output.Write(chunk.Wav.Length);
                output.Write(chunk.Wav);
            }
        }
        finally { chunker.Dispose(); }
    }
    else
    {
        throw new ArgumentException("Usage: ReferenceAudio protect | unprotect | wrap | chunk fragment [restore-byte]");
    }
    output.Flush();
    return 0;
}
catch (Exception ex) when (ex is ArgumentException or InvalidDataException or IOException or CryptographicException or PlatformNotSupportedException or JsonException or AuthCallbackException or FormatException)
{
    Console.Error.WriteLine($"Reference audio probe: {ex.Message}");
    return 2;
}
