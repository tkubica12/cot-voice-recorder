using System.Diagnostics;
using System.IO;
using System.Net.Http;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Threading;
using NAudio.Wave;
using VoicePrompt.App.Services;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;
using VoicePrompt.Core.Settings;

internal static class ControllerProbe
{
    internal static async Task RunAsync()
    {
        using var host = new ProbeHost();
        var notifications = new ProbeNotifier();
        var down = new HashSet<int>();
        var hotkey = new DictationHotkey(down.Contains);
        ReplayCapture? capture = null;
        var starts = 0;
        var desktop = new ProbeDesktop();
        var clipboard = new ProbeClipboard();
        var targetSnapshots = 0;
        await using var controller = new DictationController(host, notifications, Dispatcher.CurrentDispatcher,
            hotkey, () => { starts++; return capture = new ReplayCapture(); },
            (text, ct) => DictationDelivery.DeliverAsync(text, desktop, new ClipboardCopier(clipboard), ct),
            () => { targetSnapshots++; return desktop; }, TimeSpan.FromMilliseconds(700));
        controller.Configure();

        void Message(int id = 0x5640) => SendMessage(hotkey.Handle, 0x0312, new IntPtr(id),
            new IntPtr(id == 0x5641 ? 0x1B << 16 : (0x86 << 16) | (id == hotkey.ToggleRegistrationId ? 7 : 3)));
        async Task Release()
        {
            down.Clear();
            await Task.Delay(70);
        }
        void Press()
        {
            down.UnionWith([0x11, 0x12, 0x86]);
            Message();
        }
        void TogglePress()
        {
            down.UnionWith([0x11, 0x12, 0x10, 0x86]);
            Message(hotkey.ToggleRegistrationId);
        }

        Press();
        for (var frame = 0; frame < 40; frame++)
        {
            Message();
            await Task.Run(() => capture!.Emit(VoiceFrame()));
            await Task.Delay(15);
            Require(starts == 1 && controller.IsRecording, "Held shortcut restarted/stopped capture.");
        }
        await Release();
        Require(!controller.IsRecording, "Releasing the shortcut did not stop.");
        for (var repeat = 0; repeat < 40; repeat++)
        {
            Message();
            await Task.Delay(15);
        }
        await UntilAsync(() => !controller.IsBusy);
        Require(starts == 1 && desktop.Pastes == 1 && clipboard.Text == ProbeHost.Transcript,
            "Recording was not delivered exactly once, or stop-repeat restarted capture.");
        Require(host.History.All().Count == 1 && notifications.Messages.Count == 0,
            "Successful recording produced warnings or incorrect history.");
        Require(capture!.Stops == 1 && capture.Disposed, "Capture was not stopped/disposed exactly once.");
        Console.WriteLine("PASS controller: push-to-talk hold/release, repeated messages, worker audio, transcription, history, one delivery.");

        await Release();
        Press();
        await Task.Run(() => capture!.Emit(VoiceFrame()));
        down.Add(0x1B);
        Message(0x5641);
        await UntilAsync(() => !controller.IsBusy);
        Require(desktop.Pastes == 1 && host.History.All().Count == 1, "Cancelled capture delivered text.");
        Require(host.Recovery.List().Count == 0, "Explicit cancellation kept recovery audio.");

        await Release();
        Press();
        await Task.Run(() => capture!.Emit(new byte[1280]));
        await Release();
        for (var repeat = 0; repeat < 20; repeat++)
        {
            Message();
            await Task.Delay(15);
        }
        await UntilAsync(() => !controller.IsBusy);
        Require(starts == 3 && notifications.Messages.Count == 1
            && notifications.Messages[0].Contains("No speech detected"), "Silent stop caused a notification/restart storm.");

        await Release();
        Press();
        await Task.Run(() => capture!.Emit(VoiceFrame()));
        desktop.TargetUnchanged = false;
        down.Remove(0x11);
        await UntilAsync(() => !controller.IsBusy);
        Require(desktop.Pastes == 1 && host.History.All().Count == 2, "Focus change pasted or lost history.");
        Require(!controller.IsRecording, "Releasing a modifier did not stop capture.");
        Message();
        Require(starts == 4, "An incomplete chord restarted capture.");

        desktop.TargetUnchanged = true;
        host.FailRequests = true;
        await Release();
        Press();
        await Task.Run(() => capture!.Emit(VoiceFrame()));
        await Release();
        await UntilAsync(() => !controller.IsBusy);
        Require(desktop.Pastes == 1 && host.History.All().Count == 2,
            "Failed cloud request pasted partial text.");
        Require(notifications.Messages.Count == 3, "Unexpected duplicate error notifications.");
        var pending = host.Recovery.List().Single();
        Require(pending.PendingChunks == 1, "Cloud failure lost durable pending audio.");
        host.FailRequests = false;
        host.RecoveryContext = "different account/backend";
        try
        {
            await controller.RecoverAsync(pending.Id);
            throw new InvalidOperationException("Recovery accepted a changed account/backend.");
        }
        catch (InvalidOperationException ex) when (ex.Message.Contains("original signed-in account")) { }
        Require(host.Recovery.List().Count == 1, "Wrong-account recovery removed saved audio.");
        host.RecoveryContext = ProbeHost.DefaultContext;
        await controller.RecoverAsync(pending.Id);
        Require(desktop.Pastes == 1 && host.History.All().Count == 3 && host.Recovery.List().Count == 0,
            "Recovery pasted automatically, lost text, or did not remove finished recovery data.");
        Console.WriteLine("PASS controller: explicit discard, silence, focus fallback, offline recovery, account/backend binding, no recovery paste.");

        await Release();
        var beforeSnapshots = targetSnapshots;
        TogglePress();
        for (var repeat = 0; repeat < 20; repeat++) Message(hotkey.ToggleRegistrationId);
        await Release();
        Require(controller.IsRecording, "Toggle release or repeated native messages stopped recording.");
        await Task.Run(() => capture!.Emit(VoiceFrame()));
        desktop.TargetUnchanged = false;
        Press(); // Hold shortcut must not stop or take over the hands-free recording.
        await Release();
        Require(controller.IsRecording, "Hands-free dictation stopped on release of the hold shortcut.");
        await Task.Delay(150);
        Require(targetSnapshots == beforeSnapshots + 1, "Hands-free changed destination before stop.");
        desktop.TargetUnchanged = true;
        TogglePress();
        Require(targetSnapshots == beforeSnapshots + 2, "Hands-free did not choose the destination at stop.");
        await UntilAsync(() => !controller.IsBusy);
        Require(desktop.Pastes == 2 && clipboard.Text == ProbeHost.Transcript,
            "Hands-free stop did not deliver to the selected destination.");

        await Release();
        TogglePress();
        await Release();
        await Task.Run(() => capture!.Emit(VoiceFrame()));
        await controller.InterruptAsync();
        Require(desktop.Pastes == 2 && host.Recovery.List().Count == 1,
            "System interruption pasted or discarded the recording.");
        await controller.RecoverAsync(host.Recovery.List().Single().Id);
        Require(desktop.Pastes == 2 && host.Recovery.List().Count == 0,
            "Interrupted-session recovery pasted or retained a finished journal.");
        Console.WriteLine("PASS controller: hands-free releases ignored, destination captured at stop, interruption preserves audio without paste.");

        await Release();
        controller.Start();
        var previewDuringCapture = false;
        long maxUnsavedBytes = 0;
        for (var frame = 0; frame < 300; frame++)
        {
            await Task.Run(() => capture!.Emit(VoiceFrame()));
            await Task.Delay(40);
            Require(controller.IsRecording, "Paced capture failed while writing real DPAPI checkpoints.");
            var progress = controller.Progress!;
            previewDuringCapture |= progress.Preview.Length > 0;
            maxUnsavedBytes = Math.Max(maxUnsavedBytes, progress.CapturedBytes - progress.SavedBytes);
        }
        Require(previewDuringCapture, "No recognized text was previewed while recording.");
        Require(maxUnsavedBytes <= 4 * PcmChunker.BytesPerSecond, "Checkpoint writer fell behind real-time audio.");
        var tailLatency = Stopwatch.StartNew();
        controller.Stop();
        await UntilAsync(() => !controller.IsBusy);
        Require(desktop.Pastes == 3 && host.Recovery.List().Count == 0,
            "Paced capture was not delivered or left unfinished recovery data.");
        Console.WriteLine($"PASS controller: 12-second paced DPAPI capture, live preview, bounded checkpoints; stop-to-complete {tailLatency.ElapsedMilliseconds} ms (controlled transcription).");
    }

    internal static int RunEditor(byte[] pcm, Func<byte[], CancellationToken, Task<string>> transcribe)
    {
        using var host = new ProbeHost(transcribe);
        var app = new System.Windows.Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        var status = new TextBlock { Text = "Synthetic audio only. This test pastes into its own editor; it never submits text.",
            TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 10) };
        var editor = new System.Windows.Controls.TextBox { Name = "DictationResult", AcceptsReturn = true,
            MinHeight = 170, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        var run = new System.Windows.Controls.Button { Content = "Run cloud-to-editor test", Margin = new Thickness(0, 10, 0, 0) };
        var panel = new StackPanel { Margin = new Thickness(18) };
        panel.Children.Add(status);
        panel.Children.Add(editor);
        panel.Children.Add(run);
        var window = new Window { Title = "VoicePrompt dictation acceptance test", Width = 720, Height = 350,
            Content = panel, ShowActivated = false };
        var notifications = new ProbeNotifier();
        ReplayCapture? capture = null;
        var hotkey = new DictationHotkey();
        var controller = new DictationController(host, notifications, Dispatcher.CurrentDispatcher,
            hotkey, () => capture = new ReplayCapture());
        run.Click += async (_, _) =>
        {
            run.IsEnabled = false;
            try
            {
                Require(GetForegroundWindow() == new WindowInteropHelper(window).Handle,
                    "Focus this test window before starting.");
                editor.Text = "BEGIN: ";
                editor.CaretIndex = editor.Text.Length;
                editor.Focus();
                await Dispatcher.Yield(DispatcherPriority.Background);
                controller.Start();
                Require(controller.IsRecording, "Controller did not start.");
                status.Text = "Listening to the synthetic sample...";
                for (var offset = 0; offset < pcm.Length; offset += 1280)
                {
                    var buffer = pcm.AsSpan(offset, Math.Min(1280, pcm.Length - offset)).ToArray();
                    await Task.Run(() => capture!.Emit(buffer));
                    await Task.Delay(40);
                    Require(controller.IsRecording, "Recording ended before the sample finished.");
                }
                var watch = Stopwatch.StartNew();
                controller.Stop();
                status.Text = "Waiting for transcription and real clipboard/SendInput delivery...";
                await UntilAsync(() => !controller.IsBusy);
                await Task.Delay(200);
                var expected = host.History.Latest()?.Body;
                Require(expected is not null && expected.Contains("dictation", StringComparison.OrdinalIgnoreCase)
                    && expected.Contains("cloud", StringComparison.OrdinalIgnoreCase), "Cloud transcript did not match the sample.");
                Require(editor.Text == "BEGIN: " + expected, "Actual editor text does not equal one pasted transcript.");
                Require(notifications.Messages.Count == 0, string.Join("; ", notifications.Messages));
                status.Text = $"PASS: real cloud transcription pasted exactly once into this editor. Stop-to-check {watch.ElapsedMilliseconds} ms. No Enter sent.";
                Console.WriteLine(status.Text);
            }
            catch (Exception ex)
            {
                controller.Cancel();
                status.Text = "FAIL: " + ex.Message;
                Console.Error.WriteLine(status.Text);
            }
            finally { run.IsEnabled = true; }
        };
        var closing = false;
        window.Closing += async (_, e) =>
        {
            if (closing) return;
            e.Cancel = true;
            closing = true;
            await controller.DisposeAsync();
            app.Shutdown();
        };
        app.Run(window);
        return 0;
    }

    private static byte[] VoiceFrame()
    {
        var pcm = new byte[1280];
        for (var i = 0; i < pcm.Length; i += 2) { pcm[i] = 0xB0; pcm[i + 1] = 0x04; }
        return pcm;
    }

    private static async Task UntilAsync(Func<bool> condition)
    {
        var timeout = Stopwatch.StartNew();
        while (!condition())
        {
            Require(timeout.Elapsed < TimeSpan.FromSeconds(35), "Controller did not settle within 35 seconds.");
            await Task.Delay(20);
        }
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private sealed class ReplayCapture : IWaveIn
    {
        public WaveFormat WaveFormat { get; set; } = new(16000, 16, 1);
        public event EventHandler<WaveInEventArgs>? DataAvailable;
        public event EventHandler<StoppedEventArgs>? RecordingStopped;
        public int Stops { get; private set; }
        public bool Disposed { get; private set; }
        private bool _running;
        public void StartRecording() => _running = true;
        public void Emit(byte[] pcm)
        {
            if (_running) DataAvailable?.Invoke(this, new WaveInEventArgs(pcm, pcm.Length));
        }
        public void StopRecording()
        {
            Stops++;
            _running = false;
            _ = Task.Run(() => RecordingStopped?.Invoke(this, new StoppedEventArgs()));
        }
        public void Dispose() { Disposed = true; _running = false; }
    }

    private sealed class ProbeHost : IDictationHost, IDisposable
    {
        internal const string Transcript = "Dictation through the cloud.";
        private readonly string _root = Path.Combine(Path.GetTempPath(), "VoicePrompt-probe-" + Guid.NewGuid().ToString("N"));
        private readonly Func<byte[], CancellationToken, Task<string>>? _transcribe;
        public ProbeHost(Func<byte[], CancellationToken, Task<string>>? transcribe = null)
        {
            _transcribe = transcribe;
            Directory.CreateDirectory(_root);
            History = new HistoryStore(Path.Combine(_root, "history.json"), PhysicalFileSystem.Instance, Clock);
            Recovery = new RecoveryStore(Path.Combine(_root, "recovery"), new DpapiSecretProtector(), Clock);
        }
        public AppSettings Settings { get; } = new()
        {
            DictationEnabled = true, DictationShortcut = "Ctrl+Alt+F23",
            DictationToggleShortcut = "Ctrl+Alt+Shift+F23",
        };
        internal const string DefaultContext = "https://example.test\nprobe";
        public string RecoveryContext { get; set; } = DefaultContext;
        public RecoveryStore Recovery { get; }
        public bool CanDictate => true;
        public bool DictationBusy { get; set; }
        public bool FailRequests { get; set; }
        public HistoryStore History { get; }
        public IClock Clock => SystemClock.Instance;
        public ILog Log => NullLog.Instance;
        public async Task<string> TranscribeAsync(byte[] wav, string language, CancellationToken ct)
        {
            if (_transcribe is not null) return await _transcribe(wav, ct);
            await Task.Delay(80, ct);
            if (FailRequests) throw new HttpRequestException("Injected transcription failure.");
            return Transcript;
        }
        public void Dispose()
        {
            foreach (var item in Recovery.List()) Recovery.Delete(item.Id);
            Directory.Delete(Path.Combine(_root, "recovery"));
            File.Delete(Path.Combine(_root, "history.json"));
            Directory.Delete(_root);
        }
    }

    private sealed class ProbeNotifier : INotifier
    {
        public List<string> Messages { get; } = [];
        public void Notify(string title, string message, NotificationKind kind = NotificationKind.Info) => Messages.Add(message);
    }

    private sealed class ProbeDesktop : IDictationDesktop
    {
        public bool TargetUnchanged { get; set; } = true;
        public bool IsTargetUnchanged => TargetUnchanged;
        public bool AreModifiersReleased => true;
        public int Pastes { get; private set; }
        public bool TrySendPaste() { Pastes++; return true; }
    }

    private sealed class ProbeClipboard : IClipboardWriter
    {
        public string? Text { get; private set; }
        public void SetText(string text) => Text = text;
    }

    [DllImport("user32.dll")] private static extern IntPtr SendMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
}
