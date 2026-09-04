namespace VoicePrompt.Core.Infrastructure;

/// <summary>
/// Ensures only one tray instance runs per Windows user. A <c>Local\</c>-scoped named mutex
/// keeps the guard inside the user's session (so a second user on the same machine may run
/// their own instance), and a named event lets the second process ask the first to show its
/// window instead of silently exiting.
/// </summary>
public sealed class SingleInstanceGuard : IDisposable
{
    public const string DefaultName = "VoicePrompt.SingleInstance";

    private readonly Mutex _mutex;
    private readonly EventWaitHandle _activate;
    private readonly EventWaitHandle _quit;
    private readonly bool _owned;
    private CancellationTokenSource? _listener;

    private SingleInstanceGuard(Mutex mutex, EventWaitHandle activate, EventWaitHandle quit, bool owned)
    {
        _mutex = mutex;
        _activate = activate;
        _quit = quit;
        _owned = owned;
    }

    /// <summary>True when this process is the primary (first) instance.</summary>
    public bool IsPrimary => _owned;

    /// <summary>
    /// Acquire the guard. Never throws for the "already running" case. Ownership is decided
    /// by whether the named kernel object had to be created, not by <c>WaitOne</c> — a mutex
    /// is owned per thread, so a wait-based check would wrongly succeed twice in one process.
    /// A crashed primary releases the object automatically when its handles close.
    /// </summary>
    public static SingleInstanceGuard Acquire(string name = DefaultName)
    {
        var mutex = new Mutex(initiallyOwned: true, $"Local\\{name}.mutex", out var createdNew);
        var activate = new EventWaitHandle(false, EventResetMode.AutoReset, $"Local\\{name}.activate");
        var quit = new EventWaitHandle(false, EventResetMode.AutoReset, $"Local\\{name}.quit");
        return new SingleInstanceGuard(mutex, activate, quit, createdNew);
    }

    /// <summary>Ask the already-running primary instance to surface its window.</summary>
    public void SignalExistingInstance() => _activate.Set();

    /// <summary>
    /// Ask the already-running primary instance to shut down cleanly. Used by the installer
    /// before an upgrade and by scripted smoke tests, so a running tray app never has to be
    /// force-killed.
    /// </summary>
    public void SignalQuit() => _quit.Set();

    /// <summary>
    /// Block until the primary instance has released the guard (i.e. actually exited), or the
    /// timeout elapses. Returns <c>true</c> when the primary is gone. Lets a <c>--quit</c>
    /// helper act as a synchronous "stop the app" command for the installer.
    /// </summary>
    public bool WaitForPrimaryExit(TimeSpan timeout)
    {
        if (_owned)
        {
            return true;
        }

        var acquired = false;
        try
        {
            acquired = _mutex.WaitOne(timeout, exitContext: false);
        }
        catch (AbandonedMutexException)
        {
            // The primary died without releasing: it is gone, which is what we waited for.
            acquired = true;
        }

        if (acquired)
        {
            try
            {
                _mutex.ReleaseMutex();
            }
            catch
            {
                // Nothing to do; the process is about to exit anyway.
            }
        }

        return acquired;
    }

    /// <summary>
    /// Start listening for activation and quit requests from later instances. Callbacks run
    /// on a background thread; marshal to the UI thread in the handler.
    /// </summary>
    public void ListenForActivation(Action onActivate, Action? onQuit = null)
    {
        if (!_owned || _listener is not null)
        {
            return;
        }

        _listener = new CancellationTokenSource();
        var token = _listener.Token;
        var thread = new Thread(() =>
        {
            var handles = new WaitHandle[] { _activate, _quit, token.WaitHandle };
            while (!token.IsCancellationRequested)
            {
                var signalled = WaitHandle.WaitAny(handles);
                if (signalled == 2)
                {
                    return;
                }

                try
                {
                    if (signalled == 0)
                    {
                        onActivate();
                    }
                    else
                    {
                        onQuit?.Invoke();
                    }
                }
                catch
                {
                    // A handler failure must not kill the guard thread.
                }
            }
        })
        {
            IsBackground = true,
            Name = "VoicePrompt.SingleInstance",
        };
        thread.Start();
    }

    public void Dispose()
    {
        _listener?.Cancel();
        _listener?.Dispose();
        _listener = null;

        if (_owned)
        {
            try
            {
                _mutex.ReleaseMutex();
            }
            catch
            {
                // already released / abandoned
            }
        }

        _mutex.Dispose();
        _activate.Dispose();
        _quit.Dispose();
    }
}
