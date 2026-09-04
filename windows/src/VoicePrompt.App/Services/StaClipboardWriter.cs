using System.Threading;
using System.Windows;
using VoicePrompt.Core.Clipboard;
using Application = System.Windows.Application;
using Clipboard = System.Windows.Clipboard;

namespace VoicePrompt.App.Services;

/// <summary>
/// Writes to the Windows clipboard from a dedicated <b>STA</b> thread. OLE requires STA for
/// clipboard access, and the tray app's realtime receive loop runs on thread-pool threads, so
/// every write is marshalled here. Transient failures (another process holding the clipboard)
/// are surfaced as exceptions for <see cref="ClipboardCopier"/> to retry.
/// </summary>
public sealed class StaClipboardWriter : IClipboardWriter
{
    private readonly Func<bool> _hasDispatcher;

    public StaClipboardWriter(Func<bool>? hasDispatcher = null) =>
        _hasDispatcher = hasDispatcher ?? (() => Application.Current?.Dispatcher is not null);

    public void SetText(string text)
    {
        if (_hasDispatcher())
        {
            // The WPF dispatcher thread is already STA.
            Exception? failure = null;
            Application.Current!.Dispatcher.Invoke(() =>
            {
                try
                {
                    SetTextCore(text);
                }
                catch (Exception ex)
                {
                    failure = ex;
                }
            });

            if (failure is not null)
            {
                throw failure;
            }

            return;
        }

        RunOnStaThread(() => SetTextCore(text));
    }

    private static void SetTextCore(string text) =>
        // Copy=true flushes the data onto the clipboard so it survives process exit.
        Clipboard.SetDataObject(text, copy: true);

    private static void RunOnStaThread(Action action)
    {
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            try
            {
                action();
            }
            catch (Exception ex)
            {
                failure = ex;
            }
        });

        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        thread.Join();

        if (failure is not null)
        {
            throw failure;
        }
    }
}
