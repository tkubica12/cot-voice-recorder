using System.IO;
using System.Text.Json;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Clipboard;

namespace VoicePrompt.App.Services;

internal sealed record SignedOutFixture(string Root, string Ready, string Key, int Seconds)
{
    internal const string Scope = "full-client-signed-out-fixture";
    private bool _progressStarted;

    internal void TraceProgress(string phase)
    {
        if (Environment.GetEnvironmentVariable("VOICEPROMPT_FIXTURE_PROGRESS") != "1") return;
        using var output = new FileStream(Ready + ".progress",
            _progressStarted ? FileMode.Append : FileMode.CreateNew, FileAccess.Write, FileShare.Read);
        _progressStarted = true;
        using var writer = new StreamWriter(output);
        writer.WriteLine($"{DateTimeOffset.UtcNow:O} {phase}");
    }

    internal static string InstanceName(string key)
    {
        if (!Guid.TryParseExact(key, "N", out _))
            throw new ArgumentException("The fixture key must be an explicit N-format GUID.");
        return "VoicePrompt.MigrationFixture." + key;
    }

    internal static SignedOutFixture Create(string root, string ready, string key, int seconds)
    {
        _ = InstanceName(key);
        root = AppHost.ValidateSignedOutFixture(root);
        if (seconds is < 1 or > 3600 || !Path.IsPathFullyQualified(ready) || File.Exists(ready))
            throw new ArgumentException("Fixture readiness must be an unused absolute path with a bounded lifetime.");
        var path = Path.Combine(root, "migration-fixture.json");
        if (new FileInfo(path).Length > 1024) throw new InvalidOperationException("Fixture marker exceeds supported bounds.");
        using var marker = JsonDocument.Parse(File.ReadAllText(path));
        if (marker.RootElement.GetProperty("scope").GetString() != Scope
            || marker.RootElement.GetProperty("instance_key").GetString() != key
            || marker.RootElement.EnumerateObject().Count() != 2)
            throw new InvalidOperationException("Fixture ownership marker mismatch.");
        return new(root, ready, key, seconds);
    }

    internal static void ValidateHost(AppHost host)
    {
        if (host.Auth.Status.State != AuthState.NotConfigured || host.OAuthConfigured
            || host.Settings.BackendBaseUrl != "https://fixture.example.invalid"
            || host.Settings.AutoStart || host.Settings.DictationEnabled || !host.Settings.NotificationsPaused)
            throw new InvalidOperationException("Fixture must be signed out with device/clipboard/autostart paths disabled.");
    }

    internal void Publish(int historyCount)
    {
        var payload = JsonSerializer.SerializeToUtf8Bytes(new
        {
            status = "ready", scope = Scope, pid = Environment.ProcessId,
            implementation = "csharp", history_count = historyCount,
            auth = "not_configured", physical_ports_enabled = false,
            full_components = true, instance_key = Key,
        });
        var pending = Ready + "." + Guid.NewGuid().ToString("N") + ".tmp";
        using (var output = new FileStream(pending, FileMode.CreateNew, FileAccess.Write, FileShare.None))
        {
            output.Write(payload);
            output.Flush(flushToDisk: true);
        }
        File.Move(pending, Ready, overwrite: false);
    }

    internal sealed class ForbiddenClipboard : IClipboardWriter
    {
        public void SetText(string text) => throw new InvalidOperationException("Fixture cannot access the system clipboard.");
    }
}
