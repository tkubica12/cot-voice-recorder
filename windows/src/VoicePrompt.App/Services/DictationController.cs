using System.Diagnostics;
using System.IO;
using System.Threading.Channels;
using System.Windows.Threading;
using NAudio.Wave;
using VoicePrompt.App.Views;
using VoicePrompt.Core.Api;
using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.Dictation;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Notifications;

namespace VoicePrompt.App.Services;

public sealed class DictationController : IAsyncDisposable
{
    private readonly IDictationHost _host;
    private readonly INotifier _notifier;
    private readonly Dispatcher _dispatcher;
    private readonly DictationHotkey _hotkey;
    private readonly Func<IWaveIn> _captureFactory;
    private readonly Func<IDictationDesktop> _desktopFactory;
    private readonly Func<string, CancellationToken, Task<DictationDeliveryResult>> _deliver;
    private readonly TimeSpan _finishTimeout;
    private readonly DictationIndicator _indicator = new();
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromMilliseconds(100) };
    private readonly Stopwatch _duration = new();
    private IWaveIn? _microphone;
    private RecoverableDictationSession? _session;
    private IDictationDesktop? _desktop;
    private CancellationTokenSource? _cancel;
    private TaskCompletionSource? _captureStopped;
    private Channel<byte[]>? _audio;
    private Task _audioWorker = Task.CompletedTask;
    private Task _finishing = Task.CompletedTask;
    private bool _recording;
    private volatile bool _discard;
    private bool _preserve;
    private bool _toggleMode;
    private bool _recovering;
    private bool _disposed;
    private bool _busy;
    private bool _failureReported;
    private bool _terminalPresented;
    private int _overflow;
    private string? _recoveryId;

    public DictationController(AppHost host, INotifier notifier, Dispatcher dispatcher)
        : this(host, notifier, dispatcher, new DictationHotkey(), () => new WaveInEvent
        {
            DeviceNumber = -1,
            WaveFormat = new WaveFormat(PcmChunker.SampleRate, 16, 1),
            BufferMilliseconds = 40,
            NumberOfBuffers = 3,
        }) { }

    internal DictationController(IDictationHost host, INotifier notifier, Dispatcher dispatcher,
        DictationHotkey hotkey, Func<IWaveIn> captureFactory,
        Func<string, CancellationToken, Task<DictationDeliveryResult>>? deliver = null,
        Func<IDictationDesktop>? desktopFactory = null, TimeSpan? finishTimeout = null)
    {
        _host = host;
        _notifier = notifier;
        _dispatcher = dispatcher;
        _hotkey = hotkey;
        _captureFactory = captureFactory;
        _desktopFactory = desktopFactory ?? (() => new DictationDesktop());
        _finishTimeout = finishTimeout ?? TimeSpan.FromSeconds(30);
        _deliver = deliver ?? ((text, ct) => DictationDelivery.DeliverAsync(text, _desktop!,
            new ClipboardCopier(new MarkedClipboardWriter(_desktop!)), ct));
        _hotkey.Pressed += Start;
        _hotkey.Released += () => { if (!_toggleMode) Stop(); };
        _hotkey.TogglePressed += Toggle;
        _hotkey.Cancel += Cancel;
        _timer.Tick += OnTick;
    }

    public bool IsBusy => _busy;
    internal bool IsRecording => _recording;
    internal DictationProgress? Progress => _session?.Progress;
    public event Action? RecoveryChanged;

    public void Configure()
    {
        if (IsBusy) throw new InvalidOperationException("Stop dictation before changing its settings.");
        _hotkey.Configure(_host.Settings.DictationEnabled, _host.Settings.DictationShortcut,
            _host.Settings.DictationToggleShortcut);
    }

    public void Toggle()
    {
        if (_recording && _toggleMode) Stop();
        else if (!IsBusy) StartCapture(toggle: true);
    }

    public void Start() => StartCapture(toggle: false);

    public void Stop()
    {
        if (!_recording) return;
        // Hands-free dictation deliberately chooses its destination at STOP, not at START.
        if (_toggleMode) _desktop = _desktopFactory();
        _finishing = FinishAsync();
    }

    private void StartCapture(bool toggle)
    {
        if (_disposed || !_host.Settings.DictationEnabled || IsBusy) return;
        if (!_host.CanDictate)
        {
            _notifier.Notify("Dictation", "Sign in in Settings before dictating.", NotificationKind.Warning);
            return;
        }
        try
        {
            BeginSession();
            _toggleMode = toggle;
            _desktop = _desktopFactory();
            var language = _host.Settings.DictationLanguage;
            var context = _host.RecoveryContext;
            var journal = _host.Recovery.Create(language, context);
            _recoveryId = journal.Snapshot.Id;
            _session = CreateSession(journal, language, context);
            _audio = Channel.CreateBounded<byte[]>(new BoundedChannelOptions(50)
            {
                SingleReader = true,
                SingleWriter = true,
                FullMode = BoundedChannelFullMode.Wait,
            });
            _audioWorker = ConsumeAudioAsync(_audio, _session);
            _captureStopped = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _microphone = _captureFactory();
            _microphone.DataAvailable += OnAudio;
            _microphone.RecordingStopped += OnCaptureStopped;
            _recording = true;
            _microphone.StartRecording();
            _duration.Restart();
            _timer.Start();
            RenderProgress();
            _host.Log.Info($"dictation: capture started; language={language}; hands-free={toggle}");
        }
        catch (Exception ex)
        {
            ReportFailure(ex);
            _recording = false;
            _captureStopped?.TrySetResult();
            _audio?.Writer.TryComplete();
            _finishing = CleanupAsync();
        }
    }

    private void BeginSession()
    {
        _busy = _host.DictationBusy = true;
        _discard = _preserve = _failureReported = false;
        _terminalPresented = false;
        _recovering = false;
        _overflow = 0;
        _recoveryId = null;
        _cancel = new CancellationTokenSource();
        _hotkey.EnableCancel(true);
    }

    private Task<string> TranscribeAsync(byte[] wav, string language, string context, CancellationToken ct)
    {
        if (!_host.CanDictate || !string.Equals(context, _host.RecoveryContext, StringComparison.Ordinal))
            throw new ApiException(ApiErrorKind.Unauthorized, 401, null,
                "Sign in to the original account and backend to recover this dictation.");
        return _host.TranscribeAsync(wav, language, ct);
    }

    private RecoverableDictationSession CreateSession(RecoveryJournal journal, string language, string context)
    {
        try
        {
            return new RecoverableDictationSession(journal, (wav, ct) => TranscribeAsync(wav, language, context, ct));
        }
        catch
        {
            journal.Dispose();
            throw;
        }
    }

    private void OnAudio(object? sender, WaveInEventArgs e)
    {
        if (!ReferenceEquals(sender, _microphone)) return;
        var channel = _audio;
        if (_discard || channel is null || Volatile.Read(ref _overflow) != 0) return;
        var buffer = e.Buffer.AsSpan(0, e.BytesRecorded).ToArray();
        if (channel.Writer.TryWrite(buffer)) return;
        Array.Clear(buffer);
        if (Interlocked.Exchange(ref _overflow, 1) == 0)
            _dispatcher.BeginInvoke(() =>
            {
                ReportFailure(new IOException("Audio checkpoint writer cannot keep up."));
                Interrupt();
            });
    }

    private async Task ConsumeAudioAsync(Channel<byte[]> channel, RecoverableDictationSession session)
    {
        try
        {
            await foreach (var buffer in channel.Reader.ReadAllAsync().ConfigureAwait(false))
            {
                try { if (!_discard) session.Append(buffer); }
                finally { Array.Clear(buffer); }
            }
        }
        catch (Exception ex)
        {
            channel.Writer.TryComplete();
            _ = _dispatcher.BeginInvoke(() => { ReportFailure(ex); Interrupt(); });
        }
        finally
        {
            while (channel.Reader.TryRead(out var remaining)) Array.Clear(remaining);
        }
    }

    private void OnCaptureStopped(object? sender, StoppedEventArgs e)
    {
        if (!ReferenceEquals(sender, _microphone)) return;
        var stopped = _captureStopped;
        _audio?.Writer.TryComplete();
        _dispatcher.BeginInvoke(() =>
        {
            stopped?.TrySetResult();
            if (!ReferenceEquals(sender, _microphone)) return;
            if (e.Exception is not null || _recording)
            {
                ReportFailure(e.Exception ?? new IOException("The microphone stopped unexpectedly."));
                Interrupt();
            }
        });
    }

    private void OnTick(object? sender, EventArgs e)
    {
        if (_desktop is not null && (!_toggleMode || !_recording)) _ = _desktop.IsTargetUnchanged;
        if (_session?.Progress.Failure is { } failure && !_preserve && !_discard)
        {
            ReportFailure(failure);
            Interrupt();
        }
        RenderProgress();
    }

    private void RenderProgress()
    {
        if (_session is null || _discard || _preserve) return;
        var progress = _session.Progress;
        var title = _recording
            ? $"Listening {(int)_duration.Elapsed.TotalMinutes:00}:{_duration.Elapsed.Seconds:00} - "
                + (_toggleMode ? "press toggle to finish" : "release to finish")
            : "Finishing - Esc discards";
        var saved = FormatAudioTime(progress.SavedBytes);
        var status = progress.ServiceError is not null
            ? $"Audio saved through {saved}; cloud unavailable. Retry from Recovery."
            : $"Audio saved through {saved} | Transcribed through {FormatAudioTime(progress.TranscribedBytes)}"
                + (progress.PendingChunks > 0 ? $" | {progress.PendingChunks} pending" : "");
        _indicator.Present(title, recording: _recording, preview: progress.Preview, status: status);
    }

    private static string FormatAudioTime(long bytes)
    {
        var time = TimeSpan.FromSeconds((double)bytes / PcmChunker.BytesPerSecond);
        return $"{(int)time.TotalMinutes:00}:{time.Seconds:00}";
    }

    public void Cancel()
    {
        if (!IsBusy || _discard) return;
        if (_recovering)
        {
            _cancel?.Cancel();
            return;
        }
        _discard = true;
        _cancel?.Cancel();
        _indicator.Hide();
        if (_recording) _finishing = FinishAsync();
        _host.Log.Info("dictation: explicit discard; no paste");
    }

    public void Interrupt() => _ = InterruptAsync();

    public Task InterruptAsync()
    {
        if (!IsBusy) return Task.CompletedTask;
        _preserve = true;
        _cancel?.Cancel();
        _indicator.Hide();
        if (_recording) _finishing = FinishAsync();
        return _finishing;
    }

    private async Task FinishAsync()
    {
        var tailLatency = Stopwatch.StartNew();
        _recording = false;
        _duration.Stop();
        _host.Log.Info($"dictation: stop requested; recording-ms={_duration.ElapsedMilliseconds}; hands-free={_toggleMode}");
        RenderProgress();
        try
        {
            _microphone!.StopRecording();
            await _captureStopped!.Task;
            await _audioWorker;
            if (_discard) return;
            await Task.Run(() => _session!.FinishCapture());
            var progress = _session!.Progress;
            _host.Log.Info($"dictation: checkpoint complete; pcm-bytes={progress.CapturedBytes}; pending-chunks={progress.PendingChunks}");
            if (_preserve) return;
            var text = await _session!.WaitForCompletionAsync(_finishTimeout, _cancel!.Token);
            _cancel.Token.ThrowIfCancellationRequested();
            if (text is null)
            {
                _notifier.Notify("Dictation saved for recovery",
                    "Cloud transcription is still pending. Open Recovery to finish later; nothing was pasted.",
                    NotificationKind.Warning);
                return;
            }
            if (string.IsNullOrWhiteSpace(text))
            {
                _discard = true;
                _notifier.Notify("Dictation", "No speech detected. Nothing was pasted.");
                return;
            }
            SaveHistory(text);
            var result = await _deliver(text, _cancel.Token);
            _host.Log.Info($"dictation: {result}; stop-to-delivery-ms={tailLatency.ElapsedMilliseconds}");
            // History is the durable fallback even if the clipboard is locked or paste is refused.
            _discard = true;
            _timer.Stop();
            _hotkey.EnableCancel(false);
            _indicator.Present(result == DictationDeliveryResult.PasteSent ? "Pasted" : "Saved - paste skipped",
                recording: false, preview: RecoverableDictationSession.PreviewTail(text),
                status: result == DictationDeliveryResult.ClipboardFailed ? "Copy from History." : "Text is on the clipboard and in History.",
                dismiss: true);
            _terminalPresented = true;
            if (result != DictationDeliveryResult.PasteSent)
                _notifier.Notify("Dictation saved", result == DictationDeliveryResult.ClipboardFailed
                    ? "Clipboard busy. Use History to copy the dictation."
                    : "Automatic paste was skipped. Text is on the clipboard and in History.", NotificationKind.Warning);
        }
        catch (OperationCanceledException) when (_cancel?.IsCancellationRequested == true) { }
        catch (Exception ex) { ReportFailure(ex); }
        finally { await CleanupAsync(); }
    }

    public Task RecoverAsync(string id)
    {
        if (_disposed || IsBusy) throw new InvalidOperationException("Stop dictation before recovering a session.");
        _finishing = RecoverCoreAsync(id);
        return _finishing;
    }

    private async Task RecoverCoreAsync(string id)
    {
        try
        {
            BeginSession();
            _recovering = true;
            _preserve = true; // Recovery is explicitly never an automatic paste operation.
            var journal = await Task.Run(() => _host.Recovery.Open(id));
            var state = journal.Snapshot;
            if (!_host.CanDictate || state.Context != _host.RecoveryContext)
            {
                journal.Dispose();
                throw new InvalidOperationException("Use the original signed-in account and backend for recovery.");
            }
            _recoveryId = id;
            _session = CreateSession(journal, state.Language, state.Context);
            await Task.Run(() => _session.FinishCapture());
            var text = await _session.WaitForCompletionAsync(_finishTimeout, _cancel!.Token, resetTimeoutOnProgress: true);
            _cancel.Token.ThrowIfCancellationRequested();
            if (text is null) throw new IOException("Transcription is still pending. Saved audio is intact; retry later.");
            if (!string.IsNullOrWhiteSpace(text)) SaveHistory(text);
            _discard = true;
            _notifier.Notify("Dictation recovered", string.IsNullOrWhiteSpace(text)
                ? "No speech detected in the recovered audio."
                : "Saved to History. Select it and Copy when ready; nothing was pasted.");
        }
        finally { await CleanupAsync(); }
    }

    private void SaveHistory(string text)
    {
        var id = "dictation-" + _recoveryId;
        _host.History.Add(new HistoryEntry
        {
            TranscriptId = id, RecordingId = id, Body = text,
            Preview = string.Concat(text.EnumerateRunes().Take(160).Select(r => r.ToString())),
            CompletedAt = _host.Clock.UtcNow, CachedAt = _host.Clock.UtcNow, CharacterCount = text.Length,
        });
    }

    private void ReportFailure(Exception ex)
    {
        if (_failureReported) return;
        _failureReported = true;
        _host.Log.Warn($"dictation: failed ({ex.GetType().Name})"
            + (ex is ApiException api ? $" status={api.StatusCode}" : ""));
        _notifier.Notify("Dictation interrupted",
            "Nothing was pasted. Previously saved audio/text is retained in Recovery. Check microphone, disk space and connection.",
            NotificationKind.Error);
    }

    private async Task CleanupAsync()
    {
        _timer.Stop();
        _duration.Stop();
        if (!_terminalPresented) _indicator.Hide();
        _hotkey.EnableCancel(false);
        _audio?.Writer.TryComplete();
        await _audioWorker;
        if (_microphone is not null)
        {
            _microphone.DataAvailable -= OnAudio;
            _microphone.RecordingStopped -= OnCaptureStopped;
            _microphone.Dispose();
            _microphone = null;
        }
        if (_session is not null)
        {
            await _session.DisposeAsync();
            _session = null;
        }
        if (_discard && _recoveryId is not null)
        {
            try { await Task.Run(() => _host.Recovery.Delete(_recoveryId)); }
            catch (Exception ex)
            {
                _host.Log.Warn($"dictation: recovery deletion failed ({ex.GetType().Name})");
                _notifier.Notify("Recovery cleanup failed", "Saved recovery data could not be removed. Retry Discard in Recovery.",
                    NotificationKind.Warning);
            }
        }
        _cancel?.Dispose();
        _cancel = null;
        _audio = null;
        _audioWorker = Task.CompletedTask;
        _captureStopped = null;
        _desktop = null;
        _recording = _busy = _host.DictationBusy = false;
        RecoveryChanged?.Invoke();
    }

    public async ValueTask DisposeAsync()
    {
        _disposed = true;
        try
        {
            await InterruptAsync();
            await _finishing;
        }
        catch (Exception ex) { _host.Log.Warn($"dictation: shutdown observed failure ({ex.GetType().Name})"); }
        finally
        {
            _hotkey.Dispose();
            _indicator.Close();
        }
    }

    private sealed class MarkedClipboardWriter(IDictationDesktop desktop) : IClipboardWriter
    {
        private readonly StaClipboardWriter _writer = new();
        public void SetText(string text)
        {
            _writer.SetText(text);
            if (desktop is DictationDesktop native) native.MarkClipboard();
        }
    }
}
