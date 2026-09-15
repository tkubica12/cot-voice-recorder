using System.Diagnostics;
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
    public const int MaxRecordingSeconds = 300;
    private readonly IDictationHost _host;
    private readonly INotifier _notifier;
    private readonly Dispatcher _dispatcher;
    private readonly DictationHotkey _hotkey;
    private readonly Func<IWaveIn> _captureFactory;
    private readonly Func<string, CancellationToken, Task<DictationDeliveryResult>> _deliver;
    private readonly DictationIndicator _indicator = new();
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromMilliseconds(100) };
    private readonly Stopwatch _duration = new();
    private IWaveIn? _microphone;
    private PcmChunker? _chunker;
    private DictationSession? _session;
    private DictationDesktop? _desktop;
    private CancellationTokenSource? _cancel;
    private TaskCompletionSource? _captureStopped;
    private Task _finishing = Task.CompletedTask;
    private bool _recording;
    private bool _discard;
    private bool _disposed;
    private bool _busy;
    private long _capturedBytes;
    private int _emittedChunks;

    public DictationController(AppHost host, INotifier notifier, Dispatcher dispatcher)
        : this(host, notifier, dispatcher, new DictationHotkey(), () => new WaveInEvent
        {
            DeviceNumber = -1,
            WaveFormat = new WaveFormat(PcmChunker.SampleRate, 16, 1),
            BufferMilliseconds = 40,
            NumberOfBuffers = 3,
        })
    {
    }

    internal DictationController(IDictationHost host, INotifier notifier, Dispatcher dispatcher,
        DictationHotkey hotkey, Func<IWaveIn> captureFactory,
        Func<string, CancellationToken, Task<DictationDeliveryResult>>? deliver = null)
    {
        _host = host;
        _notifier = notifier;
        _dispatcher = dispatcher;
        _hotkey = hotkey;
        _captureFactory = captureFactory;
        _deliver = deliver ?? ((text, ct) => DictationDelivery.DeliverAsync(text, _desktop!,
            new ClipboardCopier(new MarkedClipboardWriter(_desktop!)), ct));
        _hotkey.Pressed += Start;
        _hotkey.Released += Stop;
        _hotkey.Cancel += Cancel;
        _timer.Tick += OnTick;
    }

    public bool IsBusy => _busy;
    internal bool IsRecording => _recording;

    public void Configure()
    {
        if (IsBusy) throw new InvalidOperationException("Stop dictation before changing its settings.");
        _hotkey.Configure(_host.Settings.DictationEnabled, _host.Settings.DictationShortcut);
    }

    public void Stop()
    {
        if (_recording) _finishing = FinishAsync();
    }

    public void Start()
    {
        if (_disposed || !_host.Settings.DictationEnabled || IsBusy) return;
        if (!_host.CanDictate)
        {
            _notifier.Notify("Dictation", "Sign in in Settings before dictating.", NotificationKind.Warning);
            return;
        }
        try
        {
            _busy = true;
            _host.DictationBusy = true;
            _desktop = new DictationDesktop();
            _hotkey.EnableCancel(true);
            _cancel = new CancellationTokenSource();
            _chunker = new PcmChunker();
            _capturedBytes = 0;
            _emittedChunks = 0;
            var language = _host.Settings.DictationLanguage;
            _session = new DictationSession(
                (wav, ct) => _host.TranscribeAsync(wav, language, ct));
            _captureStopped = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _microphone = _captureFactory();
            _microphone.DataAvailable += OnAudio;
            _microphone.RecordingStopped += OnCaptureStopped;
            _recording = true;
            _discard = false;
            _microphone.StartRecording();
            _duration.Restart();
            _timer.Start();
            _indicator.Present("Listening - release to finish");
            _host.Log.Info($"dictation: capture started; language={language}");
        }
        catch (Exception ex)
        {
            ReportFailure(ex);
            _discard = true;
            _recording = false;
            // StartRecording may fail before a capture worker exists.
            _captureStopped?.TrySetResult();
            _finishing = CleanupAsync();
        }
    }

    private void OnAudio(object? sender, WaveInEventArgs e)
    {
        // Synchronous dispatch bounds buffered audio when the UI is busy.
        _dispatcher.Invoke(() =>
        {
            if (_discard || _chunker is null || _session is null) return;
            try
            {
                _capturedBytes += e.BytesRecorded;
                foreach (var chunk in _chunker.Append(e.Buffer.AsSpan(0, e.BytesRecorded)))
                {
                    _session.Add(chunk);
                    _emittedChunks++;
                }
            }
            catch (Exception ex)
            {
                ReportFailure(ex);
                Cancel();
            }
        });
    }

    private void OnCaptureStopped(object? sender, StoppedEventArgs e)
    {
        if (!_dispatcher.CheckAccess())
        {
            _dispatcher.BeginInvoke(() => OnCaptureStopped(sender, e));
            return;
        }
        _captureStopped?.TrySetResult();
        if (e.Exception is not null && !_discard)
        {
            ReportFailure(e.Exception);
            Cancel();
        }
        else if (_recording && !_discard)
        {
            ReportFailure(new InvalidOperationException("The microphone stopped unexpectedly."));
            Cancel();
        }
    }

    private void OnTick(object? sender, EventArgs e)
    {
        if (_desktop is not null) _ = _desktop.IsTargetUnchanged;
        if (_session?.Failure is { } failure && !_discard)
        {
            ReportFailure(failure);
            Cancel();
            return;
        }
        if (_recording)
        {
            _indicator.Present($"Listening {_duration.Elapsed:mm\\:ss} - release", _chunker?.Level ?? 0);
            if (_duration.Elapsed.TotalSeconds >= MaxRecordingSeconds)
            {
                _notifier.Notify("Dictation", "Five-minute limit reached; finishing this dictation.");
                _finishing = FinishAsync();
            }
        }
    }

    public void Cancel()
    {
        if (!IsBusy || _discard) return;
        _discard = true;
        _cancel?.Cancel();
        _session?.Cancel();
        if (_recording)
        {
            _recording = false;
            _finishing = CancelCaptureAsync();
        }
        _indicator.Hide();
        _host.Log.Info("dictation: cancelled; no paste");
    }

    private async Task CancelCaptureAsync()
    {
        try
        {
            _microphone?.StopRecording();
            if (_captureStopped is not null) await _captureStopped.Task;
        }
        catch (Exception ex)
        {
            ReportFailure(ex);
        }
        finally
        {
            await CleanupAsync();
        }
    }

    private async Task FinishAsync()
    {
        _recording = false;
        var tail = Stopwatch.StartNew();
        _host.Log.Info($"dictation: stop requested; recording-ms={_duration.ElapsedMilliseconds}");
        _indicator.Present("Transcribing - Esc cancels", recording: false);
        try
        {
            _microphone!.StopRecording();
            await _captureStopped!.Task;
            if (_discard) return;
            foreach (var chunk in _chunker!.Flush())
            {
                _session!.Add(chunk);
                _emittedChunks++;
            }
            _host.Log.Info($"dictation: capture stopped; pcm-bytes={_capturedBytes}; chunks={_emittedChunks}");
            var text = await _session!.CompleteAsync();
            _cancel!.Token.ThrowIfCancellationRequested();
            if (string.IsNullOrWhiteSpace(text))
            {
                _notifier.Notify("Dictation", _capturedBytes == 0
                    ? "No audio captured. Hold the shortcut while speaking; check your input device if this continues."
                    : "No speech detected. Nothing was pasted.");
                return;
            }
            var now = _host.Clock.UtcNow;
            var id = "dictation-" + Guid.NewGuid().ToString("N");
            _host.History.Add(new HistoryEntry
            {
                TranscriptId = id,
                RecordingId = id,
                Body = text,
                Preview = text.Length > 160 ? text[..160] : text,
                CompletedAt = now,
                CachedAt = now,
                CharacterCount = text.Length,
            });
            var result = await _deliver(text, _cancel.Token);
            _host.Log.Info($"dictation: {result}; stop-to-delivery-ms={tail.ElapsedMilliseconds}");
            if (result != DictationDeliveryResult.PasteSent)
                _notifier.Notify("Dictation saved", result == DictationDeliveryResult.ClipboardFailed
                    ? "Clipboard busy. Use History to copy the dictation."
                    : "Automatic paste was skipped (focus changed or app blocked input). Text is on the clipboard and in History.",
                    NotificationKind.Warning);
        }
        catch (OperationCanceledException) when (_discard || _cancel?.IsCancellationRequested == true)
        {
            // Explicit cancellation never pastes.
        }
        catch (Exception ex)
        {
            ReportFailure(ex);
        }
        finally
        {
            await CleanupAsync();
        }
    }

    private void ReportFailure(Exception ex)
    {
        _host.Log.Warn($"dictation: failed ({ex.GetType().Name})"
            + (ex is ApiException error ? $" status={error.StatusCode}" : ""));
        var message = ex is ApiException api ? api.Kind switch
        {
            ApiErrorKind.Unauthorized => "Sign in in Settings before dictating.",
            ApiErrorKind.Forbidden => "This account is not allowed to transcribe.",
            ApiErrorKind.NotFound => "The backend needs the dictation update. Nothing was pasted.",
            _ => "Cloud transcription failed. Nothing was pasted; please try again.",
        } : ex is System.ComponentModel.Win32Exception
            ? ex.Message
            : "Dictation failed (microphone, shortcut, or transcription backlog). Nothing was pasted. Check Settings and try again.";
        _notifier.Notify("Dictation failed", message, NotificationKind.Error);
    }

    private async Task CleanupAsync()
    {
        _timer.Stop();
        _duration.Stop();
        _indicator.Hide();
        _hotkey.EnableCancel(false);
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
        _cancel?.Dispose();
        _cancel = null;
        _chunker = null;
        _desktop = null;
        _recording = _busy = false;
        _host.DictationBusy = false;
    }

    public async ValueTask DisposeAsync()
    {
        _disposed = true;
        Cancel();
        await _finishing;
        _hotkey.Dispose();
        _indicator.Close();
    }

    private sealed class MarkedClipboardWriter(DictationDesktop desktop) : IClipboardWriter
    {
        private readonly StaClipboardWriter _writer = new();
        public void SetText(string text)
        {
            _writer.SetText(text);
            desktop.MarkClipboard();
        }
    }
}
