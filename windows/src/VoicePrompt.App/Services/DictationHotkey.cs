using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Threading;
using VoicePrompt.Core.Dictation;

namespace VoicePrompt.App.Services;

public sealed class DictationHotkey : IDisposable
{
    private const int ToggleId = 0x5640;
    private const int CancelId = ToggleId + 1;
    private readonly HwndSource _source = new(new HwndSourceParameters("VoicePrompt dictation shortcuts")
    {
        ParentWindow = new IntPtr(-3),
        WindowStyle = 0,
    });
    private bool _registered;
    private bool _cancelRegistered;
    private string? _shortcut;
    private int _key;
    private ModifierKeys _modifiers;
    private readonly HotkeyPressGate _togglePress = new();
    private readonly HotkeyPressGate _cancelPress = new();
    private readonly Func<int, bool> _isKeyDown;
    private readonly DispatcherTimer _releaseTimer = new() { Interval = TimeSpan.FromMilliseconds(20) };

    public DictationHotkey() : this(key => (GetAsyncKeyState(key) & 0x8000) != 0) { }

    internal DictationHotkey(Func<int, bool> isKeyDown)
    {
        _isKeyDown = isKeyDown;
        _source.AddHook(OnMessage);
        _releaseTimer.Tick += (_, _) =>
        {
            var toggleDown = IsChordDown;
            var cancelDown = _isKeyDown(0x1B);
            var released = _togglePress.ObserveKeyState(toggleDown);
            _cancelPress.ObserveKeyState(cancelDown);
            if (!toggleDown && !cancelDown) _releaseTimer.Stop();
            if (released) Released?.Invoke();
        };
    }
    public event Action? Pressed;
    public event Action? Released;
    public event Action? Cancel;
    internal IntPtr Handle => _source.Handle;

    public void Configure(bool enabled, string shortcut)
    {
        if (enabled && _registered && shortcut == _shortcut)
            return;
        if (!enabled)
        {
            if (_registered)
                UnregisterHotKey(_source.Handle, ToggleId);
            _registered = false;
            _releaseTimer.Stop();
            return;
        }
        var gesture = new KeyGestureConverter().ConvertFromInvariantString(shortcut) as KeyGesture
            ?? throw new ArgumentException("Enter a shortcut such as Ctrl+Alt+Space.");
        if ((gesture.Modifiers & (ModifierKeys.Control | ModifierKeys.Alt)) == 0
            || gesture.Key is Key.None or Key.Escape)
            throw new ArgumentException("The shortcut must include Ctrl or Alt and a non-Escape key.");
        uint modifiers = 0x4000; // MOD_NOREPEAT
        if (gesture.Modifiers.HasFlag(ModifierKeys.Alt)) modifiers |= 1;
        if (gesture.Modifiers.HasFlag(ModifierKeys.Control)) modifiers |= 2;
        if (gesture.Modifiers.HasFlag(ModifierKeys.Shift)) modifiers |= 4;
        if (gesture.Modifiers.HasFlag(ModifierKeys.Windows)) modifiers |= 8;

        // Register the replacement first; a conflict must leave the current shortcut working.
        const int replacement = ToggleId + 2;
        if (!RegisterHotKey(_source.Handle, replacement, modifiers, (uint)KeyInterop.VirtualKeyFromKey(gesture.Key)))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Shortcut is already in use. Choose another.");
        if (_registered)
            UnregisterHotKey(_source.Handle, ToggleId);
        UnregisterHotKey(_source.Handle, replacement);
        _registered = RegisterHotKey(_source.Handle, ToggleId, modifiers, (uint)KeyInterop.VirtualKeyFromKey(gesture.Key));
        if (!_registered)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not register dictation shortcut.");
        _shortcut = shortcut;
        _key = KeyInterop.VirtualKeyFromKey(gesture.Key);
        _modifiers = gesture.Modifiers;
        // Do not rely solely on native repeat suppression across capture state changes.
        _togglePress.ObserveKeyState(false);
    }

    public void EnableCancel(bool enabled)
    {
        if (enabled && !_cancelRegistered)
        {
            _cancelRegistered = RegisterHotKey(_source.Handle, CancelId, 0x4000, 0x1B);
            if (!_cancelRegistered)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Escape is in use; dictation cannot start safely.");
        }
        else if (!enabled && _cancelRegistered)
        {
            UnregisterHotKey(_source.Handle, CancelId);
            _cancelRegistered = false;
        }
    }

    private IntPtr OnMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message == 0x0312)
        {
            handled = true;
            if (_registered && wParam.ToInt32() == ToggleId && IsChordDown && _togglePress.TryPress())
            {
                _releaseTimer.Start();
                Pressed?.Invoke();
            }
            if (_cancelRegistered && wParam.ToInt32() == CancelId && _cancelPress.TryPress())
            {
                _releaseTimer.Start();
                Cancel?.Invoke();
            }
        }
        return IntPtr.Zero;
    }

    private bool IsChordDown => _isKeyDown(_key)
        && (!_modifiers.HasFlag(ModifierKeys.Control) || _isKeyDown(0x11))
        && (!_modifiers.HasFlag(ModifierKeys.Alt) || _isKeyDown(0x12))
        && (!_modifiers.HasFlag(ModifierKeys.Shift) || _isKeyDown(0x10))
        && (!_modifiers.HasFlag(ModifierKeys.Windows) || _isKeyDown(0x5B) || _isKeyDown(0x5C));

    public void Dispose()
    {
        EnableCancel(false);
        _releaseTimer.Stop();
        if (_registered) UnregisterHotKey(_source.Handle, ToggleId);
        _source.Dispose();
    }

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint key);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnregisterHotKey(IntPtr hwnd, int id);
    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int key);
}
