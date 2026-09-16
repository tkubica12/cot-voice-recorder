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
        if (args.Length == 1 && args[0] == "--refine-live")
            return RefineLiveAsync().GetAwaiter().GetResult();
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
        var hotkeysOnly = args.Length == 1 && args[0] == "--hotkeys";
        if (args.Length != 0 && !hotkeysOnly)
        {
            Console.Error.WriteLine("Usage: DictationProbe [--live synthetic.wav | --refine-live | --microphone | --editor-live synthetic.wav | --hotkeys | --hotkey-burst app.dll]");
            return 2;
        }
        try
        {
            var app = new System.Windows.Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
            RunNativeHotkeys();
            if (hotkeysOnly)
            {
                app.Shutdown();
                return 0;
            }
            Require(CultureInfo.GetCultureInfo(1033).Name == "en-US", "Windows input culture is unavailable.");
            _ = InputLanguageManager.Current.CurrentInputLanguage;
            var textData = new System.Windows.DataObject(System.Windows.DataFormats.UnicodeText, "dictation probe");
            Require((string)textData.GetData(System.Windows.DataFormats.UnicodeText) == "dictation probe",
                "WPF Unicode text data failed.");
            Console.WriteLine("PASS Windows input culture and WPF Unicode text data.");
            var focus = GetForegroundWindow();
            var indicator = new DictationIndicator();
            indicator.Present("Dictation test", 0.5);
            Require(indicator.Content is System.Windows.Controls.Border
                {
                    Child: System.Windows.Controls.StackPanel { Children.Count: 2 } panel
                }
                && panel.Children[0] is System.Windows.Controls.StackPanel
                && panel.Children[1] is System.Windows.Controls.TextBlock
                {
                    Height: 60, LineHeight: 20, TextWrapping: TextWrapping.Wrap
                }
                && indicator.Height == 112,
                "Indicator must contain only a recording header and a three-line preview, without a diagnostic footer.");
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
            var down = new HashSet<int>();
            using var shortcut = (IDisposable)(constructor is not null
                ? constructor.Invoke([(Func<int, bool>)down.Contains]) : Activator.CreateInstance(type)!);
            var source = (HwndSource)type.GetField("_source", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(shortcut)!;
            var actions = 0;
            (type.GetEvent("Pressed") ?? type.GetEvent("Toggle"))!.AddEventHandler(shortcut, (Action)(() => actions++));
            var configure = type.GetMethod("Configure")!;
            configure.Invoke(shortcut, configure.GetParameters().Length == 3
                ? [true, "Ctrl+Alt+F23", "Ctrl+Alt+Shift+F23"] : [true, "Ctrl+Alt+F23"]);
            down.UnionWith([0x11, 0x12, 0x86]);
            for (var repeat = 0; repeat < 40; repeat++)
                SendMessage(source.Handle, 0x0312, new IntPtr(0x5640), new IntPtr((0x86 << 16) | 3));
            Console.WriteLine($"{(actions == 1 ? "PASS" : "FAIL")} hotkey burst: 40 notifications produced {actions} presses (expected 1).");
            return actions == 1 ? 0 : 1;
        }
        finally { context.Unload(); }
    }

    private static void RunNativeHotkeys()
    {
        const int control = 0x11, alt = 0x12, shift = 0x10, windows = 0x5B, escape = 0x1B;
        const int f23 = 0x86, f24 = 0x87;
        const uint holdModifiers = 3, toggleModifiers = 7;
        var down = new HashSet<int>();
        using var shortcut = new DictationHotkey(down.Contains);
        var presses = 0;
        var releases = 0;
        var toggles = 0;
        var cancels = 0;
        shortcut.Pressed += () => presses++;
        shortcut.Released += () => releases++;
        shortcut.TogglePressed += () => toggles++;
        shortcut.Cancel += () => cancels++;
        void Keys(params int[] keys)
        {
            down.Clear();
            down.UnionWith(keys);
            shortcut.ObserveKeyState();
        }
        void Notify(int id, int key, uint modifiers, int count = 1)
        {
            for (var repeat = 0; repeat < count; repeat++)
                SendMessage(shortcut.Handle, 0x0312, new IntPtr(id), new IntPtr((long)(((uint)key << 16) | modifiers)));
        }
        void Hold(int count = 1) => Notify(shortcut.HoldRegistrationId, f23, holdModifiers, count);
        void Toggle(int count = 1) => Notify(shortcut.ToggleRegistrationId, f23, toggleModifiers, count);
        void Reject(Action action, string message)
        {
            var rejected = false;
            try { action(); }
            catch (Exception ex) when (ex is ArgumentException or System.ComponentModel.Win32Exception)
            {
                rejected = true;
            }
            Require(rejected, message);
        }

        shortcut.Configure(true, "Ctrl+Alt+F23", "Ctrl+Alt+Shift+F23");
        var originalHoldId = shortcut.HoldRegistrationId;
        var originalToggleId = shortcut.ToggleRegistrationId;
        shortcut.Configure(true, "Ctrl+Alt+F23", "Ctrl+Alt+Shift+F23");
        Require(shortcut.HoldRegistrationId == originalHoldId && shortcut.ToggleRegistrationId == originalToggleId,
            "Unchanged shortcuts were re-registered.");
        Keys(control, alt, shift, f23);
        Toggle(40);
        Require(toggles == 1 && presses == 0 && releases == 0, "Toggle autorepeat produced multiple actions.");
        Keys(control, alt, f23);
        Hold(40);
        Require(presses == 0 && releases == 0, "Releasing Shift from toggle synthesized hold-to-talk.");
        Keys();
        Toggle();
        Require(toggles == 1 && releases == 0, "Toggle release/stale notification emitted an action.");
        Keys(control, alt, shift, f23);
        Toggle(40);
        Require(toggles == 2, "Second physical toggle press did not produce exactly one action.");
        Keys();

        Keys(control, alt, f23);
        Hold(40);
        Require(presses == 1 && releases == 0, "Hold press autorepeat was not suppressed.");
        Keys(control, alt, shift, f23);
        Toggle(40);
        Require(toggles == 2 && releases == 1, "Adding Shift to hold synthesized a toggle or missed hold release.");
        Keys(control, alt, f23);
        Hold(40);
        Require(presses == 1 && releases == 1, "Modifier-only transitions rearmed the hold latch.");
        Keys();
        Keys(control, alt, f23);
        Hold();
        Keys(control, alt);
        Require(presses == 2 && releases == 2, "Second physical hold press/release was not retained.");

        Keys(control, alt, f23);
        Toggle();
        Keys(control, alt, shift, f23);
        Toggle();
        Require(toggles == 2, "Incomplete toggle chord was accepted or rearmed by adding Shift.");
        Keys();
        Keys(control, f23);
        Hold();
        Require(presses == 2, "Hold chord without Alt was accepted.");
        Keys();
        Keys(control, alt, windows, f23);
        Hold();
        Require(presses == 2, "Unexpected Windows modifier was ignored.");
        Keys();
        Keys(control, alt, shift, windows, f23);
        Toggle();
        Require(toggles == 2, "Unexpected toggle modifier was ignored.");
        Keys();
        Keys(control, alt, f23);
        Notify(shortcut.HoldRegistrationId, f24, holdModifiers);
        Notify(shortcut.HoldRegistrationId, f23, toggleModifiers);
        Require(presses == 2, "Mismatched native key/modifier payload was accepted.");
        Hold();
        Require(presses == 3, "Rejected stale payload incorrectly consumed a valid press.");
        Keys();

        shortcut.EnableCancel(true);
        Notify(0x5641, escape, 0);
        Require(cancels == 0, "Stale Escape notification was accepted.");
        Keys(escape);
        Notify(0x5641, escape, 0, 40);
        shortcut.EnableCancel(false);
        shortcut.EnableCancel(true);
        Notify(0x5641, escape, 0, 40);
        Require(cancels == 1, "Cancel autorepeat gate was reset during registration changes.");
        Keys();
        Keys(escape);
        Notify(0x5641, escape, 0);
        Require(cancels == 2, "Second physical Escape press did not rearm.");
        shortcut.EnableCancel(false);
        Keys();
        Keys(control, alt, shift, f23);
        Toggle();
        var beforeCancel = toggles;
        shortcut.EnableCancel(true);
        down.Add(escape);
        Notify(0x5641, escape, 0);
        shortcut.EnableCancel(false);
        down.Remove(escape);
        shortcut.ObserveKeyState();
        Toggle(40);
        Require(toggles == beforeCancel, "Cancel/start/stop registration changes rearmed the toggle gate.");

        shortcut.Configure(false, "");
        Toggle();
        Hold();
        Require(toggles == beforeCancel && presses == 3, "Disabled shortcuts emitted an action.");
        shortcut.Configure(true, "Ctrl+Alt+F23", "Ctrl+Alt+Shift+F23");
        Toggle(40);
        Require(toggles == beforeCancel, "Re-enabling while held rearmed the toggle gate.");
        Keys();
        Keys(control, alt, shift, f23);
        Toggle();
        Require(toggles == beforeCancel + 1, "Re-enabled toggle failed after physical release.");
        Keys();

        var oldHoldId = shortcut.HoldRegistrationId;
        var oldToggleId = shortcut.ToggleRegistrationId;
        shortcut.Configure(true, "Ctrl+Shift+F23", "Alt+Shift+F23");
        Keys(control, alt, f23);
        Notify(oldHoldId, f23, holdModifiers);
        Keys();
        Keys(control, alt, shift, f23);
        Notify(oldToggleId, f23, toggleModifiers);
        Require(presses == 3 && toggles == beforeCancel + 1, "Stale IDs survived shortcut reconfiguration.");
        Keys();
        Keys(control, shift, f23);
        Notify(shortcut.HoldRegistrationId, f23, 6);
        Keys();
        Keys(alt, shift, f23);
        Notify(shortcut.ToggleRegistrationId, f23, 5);
        Require(presses == 4 && toggles == beforeCancel + 2, "Replacement shortcut pair did not work.");
        Keys();
        shortcut.Configure(true, "Ctrl+Alt+F23", "Ctrl+Alt+Shift+F23");

        Reject(() => shortcut.Configure(true, "Ctrl+Alt+F23", "Alt+Ctrl+F23"), "Identical gestures were allowed.");
        Reject(() => shortcut.Configure(true, "Ctrl+Alt+F23", "Shift+F23"), "Modifier-only shortcut restriction failed.");
        Reject(() => shortcut.Configure(true, "Ctrl+Escape", "Ctrl+Alt+Shift+F23"), "Escape was accepted as hold shortcut.");
        Reject(() => shortcut.Configure(true, "Ctrl+Alt+F23", "not-a-shortcut"), "Malformed toggle shortcut was accepted.");
        Reject(() => shortcut.Configure(true, "Ctrl+LeftShift", "Ctrl+Alt+Shift+F23"), "Modifier key was accepted as primary key.");

        using (var competitor = new HwndSource(new HwndSourceParameters("VoicePrompt hotkey conflict probe")
        {
            ParentWindow = new IntPtr(-3),
            WindowStyle = 0,
        }))
        {
            const int competingId = 0x5650;
            Require(RegisterHotKey(competitor.Handle, competingId, 0x4000 | toggleModifiers, f24),
                "Cannot reserve conflict-test shortcut Ctrl+Alt+Shift+F24.");
            try
            {
                var holdId = shortcut.HoldRegistrationId;
                var toggleId = shortcut.ToggleRegistrationId;
                Reject(() => shortcut.Configure(true, "Ctrl+Alt+F24", "Ctrl+Alt+Shift+F24"),
                    "Toggle registration conflict was not reported.");
                Require(shortcut.HoldRegistrationId == holdId && shortcut.ToggleRegistrationId == toggleId,
                    "Toggle conflict changed the old shortcut pair.");
                Require(RegisterHotKey(competitor.Handle, competingId + 1, 0x4000 | holdModifiers, f24),
                    "Failed pair leaked the staged hold registration.");
                UnregisterHotKey(competitor.Handle, competingId + 1);
                Reject(() => shortcut.Configure(true, "Ctrl+Alt+Shift+F24", "Ctrl+Shift+F24"),
                    "Hold registration conflict was not reported.");
                Require(shortcut.HoldRegistrationId == holdId && shortcut.ToggleRegistrationId == toggleId,
                    "Hold conflict changed the old shortcut pair.");
                Require(!RegisterHotKey(competitor.Handle, competingId + 2, 0x4000 | holdModifiers, f23),
                    "Old hold shortcut was not retained at native registration level.");
                Require(!RegisterHotKey(competitor.Handle, competingId + 3, 0x4000 | toggleModifiers, f23),
                    "Old toggle shortcut was not retained at native registration level.");
                Keys(control, alt, f23);
                Hold();
                Keys();
                Keys(control, alt, shift, f23);
                Toggle();
                Require(presses == 5 && toggles == beforeCancel + 3,
                    "Validation/conflict failure broke the original shortcuts.");
                Keys();
            }
            finally
            {
                for (var id = competingId; id <= competingId + 3; id++)
                    UnregisterHotKey(competitor.Handle, id);
            }
        }

        var swappedHold = shortcut.ToggleRegistrationId;
        var swappedToggle = shortcut.HoldRegistrationId;
        shortcut.Configure(true, "Ctrl+Alt+Shift+F23", "Ctrl+Alt+F23");
        Require(shortcut.HoldRegistrationId == swappedHold && shortcut.ToggleRegistrationId == swappedToggle,
            "Swapping shortcuts did not reuse the live registrations.");
        Keys(control, alt, shift, f23);
        Notify(shortcut.HoldRegistrationId, f23, toggleModifiers);
        Keys();
        Keys(control, alt, f23);
        Notify(shortcut.ToggleRegistrationId, f23, holdModifiers);
        Require(presses == 6 && toggles == beforeCancel + 4, "Swapped shortcut roles did not work.");
        Keys();

        shortcut.Configure(true, "Ctrl+Alt+F23", "Ctrl+Alt+F24");
        Keys(control, alt, f23);
        Hold();
        down.Add(f24);
        Notify(shortcut.ToggleRegistrationId, f24, holdModifiers, 40);
        Require(presses == 7 && toggles == beforeCancel + 5, "Different primary keys shared a latch.");
        down.Remove(f24);
        shortcut.ObserveKeyState();
        down.Add(f24);
        Notify(shortcut.ToggleRegistrationId, f24, holdModifiers, 40);
        Require(toggles == beforeCancel + 6, "Independent toggle key did not rearm while hold remained down.");
        Keys();
        var finalReleases = releases;
        shortcut.Configure(false, "");
        Notify(shortcut.HoldRegistrationId, f23, holdModifiers);
        Notify(shortcut.ToggleRegistrationId, f24, holdModifiers);
        Require(presses == 7 && toggles == beforeCancel + 6 && releases == finalReleases,
            "Final disable produced a shortcut callback.");
        Console.WriteLine("PASS native dual-hotkey repeats, physical release, exact chords, independent latches, stale messages, disable/reconfigure, and transactional conflicts.");
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

    private static async Task<int> RefineLiveAsync()
    {
        try
        {
            using var live = new LiveConnection();
            var samples = new[]
            {
                (Text: "Dnes nasad\u00edme novou verzi aplikace novou verzi aplikace na portu 8443. P\u0159ihl\u00e1\u0161en\u00ed ponech\u00e1me zapnut\u00e9.",
                    Repeated: "novou verzi aplikace"),
                (Text: "We will deploy the new version the new version on port 8443. Do not disable authentication.",
                    Repeated: "the new version"),
            };
            var changes = 0;
            foreach (var sample in samples)
            {
                var elapsed = Stopwatch.StartNew();
                var response = await live.Api.RefineDictationAsync(sample.Text, "", CancellationToken.None);
                var cleaned = response.Text;
                Require(cleaned.Contains("8443", StringComparison.Ordinal), "Cleanup changed a protected port.");
                Require(cleaned.Split(sample.Repeated, StringSplitOptions.None).Length == 2,
                    "Cleanup did not remove the synthetic repeated phrase exactly once.");
                if (!string.Equals(sample.Text, cleaned, StringComparison.Ordinal)) changes++;
                Console.WriteLine($"PASS authenticated deployed seam deduplication: elapsed-ms={elapsed.ElapsedMilliseconds}; edits={response.Edits.Count}; chars={cleaned.Length}");
            }
            Require(changes > 0, "Neither synthetic sample was changed; model editing was not demonstrated.");
            const string backgroundText =
                "Thiss is a synthetic dictation for a deployment test. We are checking that cleanup starts "
                + "while someone is still speaking, instead of waiting for the entire recording to finish. "
                + "Keep port 8443 unchanged and do not disable authentication. The original text must remain "
                + "available when a model request fails. We want one final paste, with no delayed replacement "
                + "after it has already arrived in the editor. These sentences contain no private information "
                + "and describe only a fictional example for testing.";
            await using var polisher = new DictationPolisher(live.Api.RefineDictationAsync);
            var firstWindow = string.Join(" ", backgroundText.Split(' ').Take(75)) + " ";
            polisher.Update(firstWindow);
            var recording = Stopwatch.StartNew();
            while (polisher.Progress.SuccessfulBlocks == 0 && polisher.Progress.FallbackBlocks == 0
                && recording.Elapsed < TimeSpan.FromSeconds(9))
                await Task.Delay(50);
            Require(polisher.Progress.SuccessfulBlocks > 0, "No real background model response completed before Stop.");
            var tail = Stopwatch.StartNew();
            polisher.StopScheduling();
            var result = await polisher.CompleteAsync(backgroundText + " This is the final sentence.",
                CancellationToken.None);
            Require(result.Text.Contains("8443", StringComparison.Ordinal)
                && result.RawText == backgroundText + " This is the final sentence.",
                "Deployed background cleanup lost source text or a protected literal.");
            Require(tail.Elapsed < TimeSpan.FromMilliseconds(250), "Background cleanup added a final wait.");
            Require(result.FallbackBlocks == 0, $"Unexpected background cleanup failure: {result.LastFailure}");
            Console.WriteLine($"PASS deployed background cleanup: stop-to-ready-ms={tail.ElapsedMilliseconds}; successful-windows={result.SuccessfulBlocks}; raw-tail-words={result.UnprocessedWords}; fallback-blocks={result.FallbackBlocks}; no clipboard/history writes.");
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"FAIL deployed refinement {ex.GetType().Name}"
                + (ex is ApiException api ? $" HTTP={api.StatusCode}" : ""));
            return 1;
        }
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern IntPtr SendMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint key);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnregisterHotKey(IntPtr hwnd, int id);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
}
