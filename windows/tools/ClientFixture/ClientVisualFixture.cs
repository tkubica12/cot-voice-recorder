using System.IO;
using System.Text.Json;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using VoicePrompt.App.Services;

internal static class ClientVisualFixture
{
    internal static int Run(VoicePrompt.App.App application, SignedOutFixture fixture)
    {
        var settings = Path.Combine(fixture.Root, "settings.json");
        var history = Path.Combine(fixture.Root, "history.json");
        var originalSettings = File.ReadAllBytes(settings);
        var originalHistory = File.ReadAllBytes(history);
        void Publish(string name, object value)
        {
            using var file = new FileStream(Path.Combine(fixture.Root, name),
                FileMode.CreateNew, FileAccess.Write, FileShare.Read);
            file.Write(JsonSerializer.SerializeToUtf8Bytes(value));
            file.Flush(flushToDisk: true);
        }
        application.Startup += (_, _) => application.Dispatcher.BeginInvoke(() =>
        {
            var window = application.ShowSignedOutFixtureWindow();
            var dpi = VisualTreeHelper.GetDpi(window);
            Publish("ui-preview-ready.json", new
            {
                scope = "reference-full-client-ui-preview", pid = Environment.ProcessId,
                window = new WindowInteropHelper(window).Handle.ToInt64(), physical_ports = false,
                auth = "not_configured", dpi = dpi.PixelsPerInchX,
                width_dips = window.ActualWidth, height_dips = window.ActualHeight,
            });
        }, DispatcherPriority.ApplicationIdle);
        var result = application.Run();
        if (!File.ReadAllBytes(settings).AsSpan().SequenceEqual(originalSettings)
            || !File.ReadAllBytes(history).AsSpan().SequenceEqual(originalHistory)
            || File.Exists(Path.Combine(fixture.Root, "tokens.bin")))
            throw new InvalidOperationException("The reference visual fixture changed its marked inputs or credentials.");
        Publish("ui-preview-closed.json", new
        {
            scope = "reference-full-client-ui-preview", pid = Environment.ProcessId,
            closed = true, physical_ports = false, settings_history_unchanged = true, token_cache_absent = true,
        });
        return result;
    }
}
