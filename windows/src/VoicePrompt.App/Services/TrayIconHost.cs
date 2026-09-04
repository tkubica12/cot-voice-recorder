using System.Drawing;
using System.IO;
using System.Windows.Forms;
using VoicePrompt.Core.Notifications;

namespace VoicePrompt.App.Services;

/// <summary>
/// Owns the notification-area icon, its context menu and balloon notifications. The app is a
/// tray app first: it starts hidden and this is the only always-present surface.
///
/// Menu: <b>Open</b> · <b>Copy latest</b> · <b>Pause notifications</b> · <b>Exit</b>.
/// Double-clicking the icon opens the settings/history window.
/// </summary>
public sealed class TrayIconHost : INotifier, IDisposable
{
    private readonly NotifyIcon _icon;
    private readonly ToolStripMenuItem _openItem;
    private readonly ToolStripMenuItem _copyItem;
    private readonly ToolStripMenuItem _pauseItem;
    private readonly ToolStripMenuItem _exitItem;

    public TrayIconHost(Icon icon)
    {
        _openItem = new ToolStripMenuItem("&Open") { ShortcutKeyDisplayString = "Double-click" };
        _copyItem = new ToolStripMenuItem("&Copy latest");
        _pauseItem = new ToolStripMenuItem("&Pause notifications") { CheckOnClick = true };
        _exitItem = new ToolStripMenuItem("E&xit");

        var menu = new ContextMenuStrip { ShowImageMargin = false };
        menu.Items.AddRange(new ToolStripItem[]
        {
            _openItem,
            _copyItem,
            _pauseItem,
            new ToolStripSeparator(),
            _exitItem,
        });

        _icon = new NotifyIcon
        {
            Icon = icon,
            Text = "VoicePrompt",
            Visible = true,
            ContextMenuStrip = menu,
        };

        _openItem.Click += (_, _) => Open?.Invoke();
        _copyItem.Click += (_, _) => CopyLatest?.Invoke();
        _pauseItem.CheckedChanged += OnPauseChanged;
        _exitItem.Click += (_, _) => ExitRequested?.Invoke();
        _icon.DoubleClick += (_, _) => Open?.Invoke();
        _icon.BalloonTipClicked += (_, _) => Open?.Invoke();

        // The default menu item is bolded and invoked on Enter.
        menu.Items[0].Font = new Font(menu.Font, FontStyle.Bold);
    }

    public event Action? Open;
    public event Action? CopyLatest;
    public event Action<bool>? PauseChanged;
    public event Action? ExitRequested;

    /// <summary>Reflect externally-changed pause state without re-raising <see cref="PauseChanged"/>.</summary>
    public void SetPausedChecked(bool paused)
    {
        if (_pauseItem.Checked == paused)
        {
            return;
        }

        _pauseItem.CheckedChanged -= OnPauseChanged;
        _pauseItem.Checked = paused;
        _pauseItem.CheckedChanged += OnPauseChanged;
    }

    /// <summary>Update the hover tooltip (max 63 chars on Windows).</summary>
    public void SetStatusText(string text)
    {
        var full = "VoicePrompt — " + text;
        _icon.Text = full.Length <= 63 ? full : full[..62];
    }

    public void SetCopyLatestEnabled(bool enabled) => _copyItem.Enabled = enabled;

    public void Notify(string title, string message, NotificationKind kind = NotificationKind.Info)
    {
        _icon.BalloonTipTitle = title;
        _icon.BalloonTipText = message;
        _icon.BalloonTipIcon = kind switch
        {
            NotificationKind.Warning => ToolTipIcon.Warning,
            NotificationKind.Error => ToolTipIcon.Error,
            _ => ToolTipIcon.Info,
        };
        _icon.ShowBalloonTip(4000);
    }

    private void OnPauseChanged(object? sender, EventArgs e) => PauseChanged?.Invoke(_pauseItem.Checked);

    /// <summary>Load the multi-resolution application icon from disk, with a safe fallback.</summary>
    public static Icon LoadAppIcon()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "Assets", "app.ico");
        try
        {
            if (File.Exists(path))
            {
                return new Icon(path, SystemInformation.SmallIconSize);
            }

            var exe = Environment.ProcessPath;
            if (!string.IsNullOrEmpty(exe))
            {
                var extracted = Icon.ExtractAssociatedIcon(exe);
                if (extracted is not null)
                {
                    return extracted;
                }
            }
        }
        catch
        {
            // fall through to the system default
        }

        return SystemIcons.Application;
    }

    public void Dispose()
    {
        _icon.Visible = false;
        _icon.ContextMenuStrip?.Dispose();
        _icon.Dispose();
    }
}
