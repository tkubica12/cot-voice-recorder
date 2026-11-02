using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using VoicePrompt.App.Views;

internal static class OverlayVisualFixture
{
    public static int Run(string directory)
    {
        if (!Path.IsPathFullyQualified(directory) || !Directory.Exists(directory))
            throw new ArgumentException("An explicit existing visual-only root is required.");
        using var marker = JsonDocument.Parse(File.ReadAllBytes(Path.Combine(directory, "overlay-fixture.json")));
        if (marker.RootElement.EnumerateObject().Count() != 1
            || marker.RootElement.GetProperty("scope").GetString() != "reference-overlay-visual-fixture"
            || Directory.EnumerateFileSystemEntries(directory).Count() != 1)
            throw new InvalidOperationException("A fresh marked reference visual-only root is required.");

        void Publish(string name, byte[] bytes)
        {
            using var file = new FileStream(Path.Combine(directory, name), FileMode.CreateNew,
                FileAccess.Write, FileShare.Read);
            file.Write(bytes);
            file.Flush(flushToDisk: true);
        }
        void Json(string name, object value) => Publish(name, JsonSerializer.SerializeToUtf8Bytes(value));
        Json("started.json", new { scope = "reference-overlay-visual-fixture", pid = Environment.ProcessId, physical_ports = false });

        var application = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        var indicator = new DictationIndicator();
        var states = new[]
        {
            (Name: "listening-empty", Header: "Listening 00:01 - release to finish", Preview: "", Recording: true),
            (Name: "listening-preview", Header: "Listening 00:02 - release to finish", Preview: "Synthetic live preview. These words grow while you speak.", Recording: true),
            (Name: "listening-wrap", Header: "Listening 00:03 - release to finish", Preview: "Synthetic continuous preview wraps over several lines. Each new recognized phrase replaces the live preview without activating another application. This deliberately long sample checks the original three-line layout and character ellipsis rather than hiding or shrinking the text. No microphone or clipboard is used.", Recording: true),
            (Name: "finishing", Header: "Finishing transcription - Esc discards", Preview: "Synthetic live preview. These words grow while you speak.", Recording: false),
            (Name: "saved", Header: "Saved - paste skipped", Preview: "Only confirmed final text is delivered.", Recording: false),
        };
        var phase = 0;
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(10) };
        void Next()
        {
            try
            {
                if (phase == states.Length)
                {
                    if (indicator.IsVisible)
                        throw new InvalidOperationException("The reference 900ms terminal dismiss did not hide.");
                    timer.Stop();
                    indicator.Close();
                    Json("result.json", new { scope = "reference-overlay-visual-fixture", complete = true, physical_ports = false, window_closed = true });
                    application.Shutdown(0);
                    return;
                }
                var state = states[phase++];
                indicator.Present(state.Header, recording: state.Recording, preview: state.Preview,
                    dismiss: state.Name == "saved");
                indicator.UpdateLayout();
                var content = (FrameworkElement)indicator.Content;
                foreach (var dpi in new[] { 96, 144, 192 })
                {
                    var width = 440 * dpi / 96;
                    var height = 112 * dpi / 96;
                    var bitmap = new RenderTargetBitmap(width, height, dpi, dpi, PixelFormats.Pbgra32);
                    bitmap.Render(content);
                    var pixels = new byte[width * height * 4];
                    bitmap.CopyPixels(pixels, width * 4, 0);
                    var name = $"{state.Name}-{dpi}";
                    Publish(name + ".bgra", pixels);
                    Json(name + ".json", new
                    {
                        width, height, dpi, pixel_format = "Pbgra32", header = state.Header,
                        preview = string.IsNullOrWhiteSpace(state.Preview) ? "Recognized words will appear here as you speak." : state.Preview,
                        recording = state.Recording,
                    });
                }
                if (phase == states.Length) timer.Interval = TimeSpan.FromMilliseconds(1100);
                timer.Start();
            }
            catch (Exception error)
            {
                timer.Stop();
                indicator.Close();
                Json("failure.json", new { error = error.GetType().Name, message = error.Message });
                Console.Error.WriteLine($"Reference overlay fixture failed: {error}");
                application.Shutdown(1);
            }
        }
        timer.Tick += (_, _) => Next();
        application.Dispatcher.BeginInvoke(Next);
        return application.Run();
    }
}
