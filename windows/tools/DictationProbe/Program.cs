using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Net.Http;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Loader;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Input;
using System.Windows.Threading;
using NAudio.Wave;
using VoicePrompt.App.Services;
using VoicePrompt.App.Views;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Settings;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        if (args.Length == 2 && args[0] == "--hotkey-burst")
            return HotkeyBurst(args[1]);
        if (args.Length == 2 && args[0] == "--live")
            return LiveAsync(args[1]).GetAwaiter().GetResult();
        if (args.Length == 1 && args[0] == "--microphone")
            return MicrophoneAsync().GetAwaiter().GetResult();
        if (args.Length == 2 && args[0] == "--editor-live")
        {
            try
            {
                using var live = new LiveConnection();
                return ControllerProbe.RunEditor(ReadPcm(args[1]),
                    (wav, ct) => live.Api.TranscribeDictationAsync(wav, "auto", ct));
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"FAIL editor probe: {ex.Message}");
                return 1;
            }
        }
        if (args.Length != 0)
        {
            Console.Error.WriteLine("Usage: DictationProbe [--live synthetic.wav | --microphone | --editor-live synthetic.wav | --hotkey-burst app.dll]");
            return 2;
        }
        try
        {
            var app = new System.Windows.Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
            Require(CultureInfo.GetCultureInfo(1033).Name == "en-US", "Windows input culture is unavailable.");
            _ = InputLanguageManager.Current.CurrentInputLanguage;
            var textData = new System.Windows.DataObject(System.Windows.DataFormats.UnicodeText, "dictation probe");
            Require((string)textData.GetData(System.Windows.DataFormats.UnicodeText) == "dictation probe",
                "WPF Unicode text data failed.");
            Console.WriteLine("PASS Windows input culture and WPF Unicode text data.");
            using var shortcut = new DictationHotkey();
            shortcut.Configure(true, "Ctrl+Alt+F23");
            shortcut.Configure(true, "Ctrl+Alt+F23");
            shortcut.Configure(true, "Ctrl+Shift+F23");
            shortcut.Configure(false, "");
            var focus = GetForegroundWindow();
            var indicator = new DictationIndicator();
            indicator.Present("Dictation test", 0.5);
            var hwnd = new WindowInteropHelper(indicator).Handle;
            var style = GetWindowLongPtr(hwnd, -20).ToInt64();
            Require((style & 0x08000000) != 0, "Indicator is not NOACTIVATE.");
            Require((style & 0x20) != 0, "Indicator is not click-through.");
            Require(!indicator.ShowInTaskbar, "Indicator appears on taskbar.");
            Require(GetForegroundWindow() == focus, "Indicator stole focus.");
            indicator.Close();
            var inputType = typeof(DictationDesktop).GetNestedType("Input", BindingFlags.NonPublic)
                ?? throw new InvalidOperationException("Native INPUT type is missing.");
            Require(Marshal.SizeOf(inputType) == 40, "Unexpected x64 INPUT layout.");
            Console.WriteLine("PASS native hotkey registration/reconfiguration and non-activating indicator.");
            Console.WriteLine($"Microphone devices available: {WaveInEvent.DeviceCount} (not recorded by this probe).");
            RunDispatcher(ControllerProbe.RunAsync);
            app.Shutdown();
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"FAIL {ex}");
            return 1;
        }
    }

    private static int HotkeyBurst(string assemblyPath)
    {
        var context = new AssemblyLoadContext("hotkey-regression", isCollectible: true);
        try
        {
            var assembly = context.LoadFromAssemblyPath(Path.GetFullPath(assemblyPath));
            var type = assembly.GetType("VoicePrompt.App.Services.DictationHotkey", throwOnError: true)!;
            var constructor = type.GetConstructor(BindingFlags.Instance | BindingFlags.NonPublic,
                null, [typeof(Func<int, bool>)], null);
            using var shortcut = (IDisposable)(constructor is not null
                ? constructor.Invoke([(Func<int, bool>)(_ => true)]) : Activator.CreateInstance(type)!);
            var source = (HwndSource)type.GetField("_source", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(shortcut)!;
            var actions = 0;
            (type.GetEvent("Pressed") ?? type.GetEvent("Toggle"))!.AddEventHandler(shortcut, (Action)(() => actions++));
            type.GetMethod("Configure")!.Invoke(shortcut, [true, "Ctrl+Alt+F23"]);
            for (var repeat = 0; repeat < 40; repeat++)
                SendMessage(source.Handle, 0x0312, new IntPtr(0x5640), IntPtr.Zero);
            Console.WriteLine($"{(actions == 1 ? "PASS" : "FAIL")} hotkey burst: 40 notifications produced {actions} toggles (expected 1).");
            return actions == 1 ? 0 : 1;
        }
        finally { context.Unload(); }
    }

    private static void RunDispatcher(Func<Task> action)
    {
        var frame = new DispatcherFrame();
        Exception? failure = null;
        Dispatcher.CurrentDispatcher.BeginInvoke(new Action(async () =>
        {
            try { await action(); }
            catch (Exception ex) { failure = ex; }
            finally { frame.Continue = false; }
        }));
        Dispatcher.PushFrame(frame);
        if (failure is not null) throw new InvalidOperationException("Controller regression failed: " + failure.Message, failure);
    }

    private static byte[] ReadPcm(string path)
    {
        using var reader = new WaveFileReader(path);
        Require(reader.WaveFormat.SampleRate == 16000 && reader.WaveFormat.BitsPerSample == 16
            && reader.WaveFormat.Channels == 1, "Probe requires mono 16 kHz PCM16.");
        var pcm = new byte[reader.Length];
        reader.ReadExactly(pcm);
        return pcm;
    }

    private sealed class LiveConnection : IDisposable
    {
        private readonly HttpClient _tokens = new() { Timeout = TimeSpan.FromSeconds(30) };
        private readonly HttpClient _http = new() { Timeout = TimeSpan.FromSeconds(25) };
        public ApiClient Api { get; }
        public LiveConnection()
        {
            var fs = PhysicalFileSystem.Instance;
            var paths = AppPaths.Default();
            var config = DesktopClientLocator.TryLoad(AppContext.BaseDirectory, paths.Root, fs, NullLog.Instance);
            var auth = new AuthManager(config, new GoogleTokenClient(_tokens),
                new TokenStore(paths.TokensFile, new DpapiSecretProtector(), fs),
                new SystemBrowser(), LoopbackAuthListenerFactory.Instance, SystemClock.Instance);
            Require(auth.Status.CanCallBackend, "Sign in to VoicePrompt before the live probe.");
            _http.BaseAddress = new Uri(new SettingsStore(paths.SettingsFile, fs).Load().BackendBaseUrl);
            Api = new ApiClient(_http, new AuthBackendCredentials(auth), new ApiRetryOptions { MaxRetries = 0 });
        }
        public void Dispose() { _http.Dispose(); _tokens.Dispose(); }
    }

    private static async Task<int> MicrophoneAsync()
    {
        try
        {
            using var microphone = new WaveInEvent
            {
                DeviceNumber = -1,
                WaveFormat = new WaveFormat(PcmChunker.SampleRate, 16, 1),
                BufferMilliseconds = 40,
                NumberOfBuffers = 3,
            };
            var stopped = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            var bytes = 0;
            microphone.DataAvailable += (_, e) =>
            {
                Interlocked.Add(ref bytes, e.BytesRecorded);
                Array.Clear(e.Buffer);
            };
            microphone.RecordingStopped += (_, e) =>
            {
                if (e.Exception is not null) stopped.TrySetException(e.Exception);
                else stopped.TrySetResult();
            };
            microphone.StartRecording();
            await Task.Delay(240);
            microphone.StopRecording();
            await stopped.Task.WaitAsync(TimeSpan.FromSeconds(3));
            Require(bytes >= 1280 && bytes % 2 == 0, "No complete PCM buffer received.");
            Console.WriteLine($"PASS default microphone capture/stop: bytes={bytes}; discarded, not uploaded or saved.");
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"FAIL microphone {ex.GetType().Name}: {ex.Message}");
            return 1;
        }
    }

    private static async Task<int> LiveAsync(string path)
    {
        try
        {
            using var live = new LiveConnection();
            var api = live.Api;
            var pcm = ReadPcm(path);
            for (var run = 0; run < 3; run++)
            {
                var chunker = new PcmChunker();
                var chunks = chunker.Append(pcm).Concat(chunker.Flush()).ToArray();
                Require(chunks.Length > 0, "Synthetic probe audio contains no speech.");
                var requests = new List<long>();
                await using var session = new DictationSession(async (wav, ct) =>
                {
                    var watch = Stopwatch.StartNew();
                    var text = await api.TranscribeDictationAsync(wav, "auto", ct);
                    lock (requests) requests.Add(watch.ElapsedMilliseconds);
                    return text;
                });
                var total = Stopwatch.StartNew();
                foreach (var chunk in chunks) session.Add(chunk);
                var result = await session.CompleteAsync();
                Require(result.Contains("dictation", StringComparison.OrdinalIgnoreCase)
                    && result.Contains("cloud", StringComparison.OrdinalIgnoreCase),
                    "Synthetic speech did not transcribe the expected keywords.");
                Console.WriteLine($"PASS live run={run + 1} chunks={chunks.Length} chars={result.Length} "
                    + $"total-ms={total.ElapsedMilliseconds} request-ms={string.Join(',', requests)}");
            }
            // Stream the same synthetic audio at capture cadence to measure the actual tail.
            var streamingChunker = new PcmChunker();
            await using var streaming = new DictationSession(
                (wav, ct) => api.TranscribeDictationAsync(wav, "auto", ct));
            const int frame = PcmChunker.BytesPerSecond / 25;
            for (var i = 0; i < pcm.Length; i += frame)
            {
                foreach (var chunk in streamingChunker.Append(pcm.AsSpan(i, Math.Min(frame, pcm.Length - i))))
                    streaming.Add(chunk);
                await Task.Delay(40);
            }
            var tail = Stopwatch.StartNew();
            foreach (var chunk in streamingChunker.Flush()) streaming.Add(chunk);
            var streamedText = await streaming.CompleteAsync();
            Require(streamedText.Contains("dictation", StringComparison.OrdinalIgnoreCase), "Streaming transcript was incorrect.");
            Console.WriteLine($"PASS paced capture stop-to-text-ms={tail.ElapsedMilliseconds} chars={streamedText.Length}");
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"FAIL live {ex.GetType().Name}"
                + (ex is ApiException api ? $" HTTP={api.StatusCode}" : $": {ex.Message}"));
            return 1;
        }
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern IntPtr SendMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
}
