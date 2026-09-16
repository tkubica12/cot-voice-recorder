using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Threading;
using VoicePrompt.Core.Dictation;

namespace VoicePrompt.App.Services;

public sealed class DictationHotkey : IDisposable
{
    private const int FirstId = 0x5640;
    private const int CancelId = FirstId + 1;
    private readonly HwndSource _source = new(new HwndSourceParameters("VoicePrompt dictation shortcuts")
    {
        ParentWindow = new IntPtr(-3),
        WindowStyle = 0,
    });
    private bool _enabled;
    private bool _cancelRegistered;
    private bool _holdActive;
    private Registration? _hold;
    private Registration? _toggle;
    private int _nextId = FirstId;
    private readonly HotkeyPressGate _holdPress = new();
    private readonly HotkeyPressGate _togglePress = new();
    private readonly HotkeyPressGate _cancelPress = new();
    private readonly Func<int, bool> _isKeyDown;
    private readonly DispatcherTimer _releaseTimer = new() { Interval = TimeSpan.FromMilliseconds(20) };

    public DictationHotkey() : this(key => (GetAsyncKeyState(key) & 0x8000) != 0) { }

    internal DictationHotkey(Func<int, bool> isKeyDown)
    {
        _isKeyDown = isKeyDown;
        _source.AddHook(OnMessage);
        _releaseTimer.Tick += (_, _) => ObserveKeyState();
    }

    public event Action? Pressed;
    public event Action? Released;
    public event Action? TogglePressed;
    public event Action? Cancel;
    internal IntPtr Handle => _source.Handle;
    internal int HoldRegistrationId => _hold?.Id ?? 0;
    internal int ToggleRegistrationId => _toggle?.Id ?? 0;

    public void Configure(bool enabled, string holdShortcut, string toggleShortcut = "Ctrl+Alt+Shift+Space")
    {
        if (!enabled)
        {
            if (_enabled)
            {
                UnregisterHotKey(_source.Handle, _hold!.Id);
                UnregisterHotKey(_source.Handle, _toggle!.Id);
            }
            _enabled = false;
            _holdActive = false;
            return;
        }

        var hold = ParseGesture(holdShortcut);
        var toggle = ParseGesture(toggleShortcut);
        if (hold == toggle)
            throw new ArgumentException("Hold-to-talk and toggle shortcuts must be different.");
        if (_enabled && _hold!.Gesture == hold && _toggle!.Gesture == toggle)
            return;

        var staged = new List<Registration>();
        Registration Stage(Gesture gesture)
        {
            // Reuse either live registration, including when swapping the two gestures.
            if (_enabled && _hold!.Gesture == gesture) return _hold;
            if (_enabled && _toggle!.Gesture == gesture) return _toggle;
            var id = _nextId++;
            if (id == CancelId) id = _nextId++;
            if (!RegisterHotKey(_source.Handle, id, gesture.NativeModifiers | 0x4000, (uint)gesture.Key))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Shortcut is already in use. Choose another.");
            var registration = new Registration(id, gesture);
            staged.Add(registration);
            return registration;
        }

        Registration nextHold;
        Registration nextToggle;
        try
        {
            nextHold = Stage(hold);
            nextToggle = Stage(toggle);
        }
        catch
        {
            foreach (var registration in staged) UnregisterHotKey(_source.Handle, registration.Id);
            throw;
        }

        // Keep staged IDs live: unregistering and registering again introduces a conflict race.
        if (_enabled)
        {
            foreach (var old in new[] { _hold!, _toggle! })
                if (old != nextHold && old != nextToggle) UnregisterHotKey(_source.Handle, old.Id);
        }
        if (_hold?.Gesture != hold) _holdActive = false;
        PrepareGate(_holdPress, _hold, nextHold);
        PrepareGate(_togglePress, _toggle, nextToggle);
        _hold = nextHold;
        _toggle = nextToggle;
        _enabled = true;
    }

    private void PrepareGate(HotkeyPressGate gate, Registration? previous, Registration next)
    {
        if (previous?.Gesture.Key != next.Gesture.Key) gate.ObserveKeyState(false);
        var down = _isKeyDown(next.Gesture.Key);
        gate.ObserveKeyState(down);
        if (down)
        {
            gate.TryPress();
            _releaseTimer.Start();
        }
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

    internal void ObserveKeyState()
    {
        var holdDown = _hold is not null && _isKeyDown(_hold.Gesture.Key);
        var toggleDown = _toggle is not null && _isKeyDown(_toggle.Gesture.Key);
        var cancelDown = _isKeyDown(0x1B);
        _holdPress.ObserveKeyState(holdDown);
        _togglePress.ObserveKeyState(toggleDown);
        _cancelPress.ObserveKeyState(cancelDown);
        if (!holdDown && !toggleDown && !cancelDown) _releaseTimer.Stop();
        if (_holdActive && (!_enabled || !IsChordDown(_hold!.Gesture)))
        {
            _holdActive = false;
            Released?.Invoke();
        }
    }

    private IntPtr OnMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message != 0x0312) return IntPtr.Zero;
        var id = wParam.ToInt32();
        if (_enabled && (id == _hold!.Id || id == _toggle!.Id))
        {
            handled = true;
            var isHold = id == _hold.Id;
            var registration = isHold ? _hold : _toggle!;
            if (!registration.Gesture.MatchesMessage(lParam) || !_isKeyDown(registration.Gesture.Key))
                return IntPtr.Zero;
            var gate = isHold ? _holdPress : _togglePress;
            var firstPress = gate.TryPress();
            // Modifier changes while the primary key stays down are not new physical presses.
            if (_hold.Gesture.Key == _toggle!.Gesture.Key)
                (isHold ? _togglePress : _holdPress).TryPress();
            _releaseTimer.Start();
            if (firstPress && IsChordDown(registration.Gesture))
            {
                if (isHold)
                {
                    _holdActive = true;
                    Pressed?.Invoke();
                }
                else TogglePressed?.Invoke();
            }
        }
        else if (_cancelRegistered && id == CancelId)
        {
            handled = true;
            if (new Gesture(0x1B, ModifierKeys.None).MatchesMessage(lParam)
                && _isKeyDown(0x1B) && _cancelPress.TryPress())
            {
                _releaseTimer.Start();
                Cancel?.Invoke();
            }
        }
        return IntPtr.Zero;
    }

    private bool IsChordDown(Gesture gesture) => _isKeyDown(gesture.Key)
        && gesture.Modifiers.HasFlag(ModifierKeys.Control) == _isKeyDown(0x11)
        && gesture.Modifiers.HasFlag(ModifierKeys.Alt) == _isKeyDown(0x12)
        && gesture.Modifiers.HasFlag(ModifierKeys.Shift) == _isKeyDown(0x10)
        && gesture.Modifiers.HasFlag(ModifierKeys.Windows) == (_isKeyDown(0x5B) || _isKeyDown(0x5C));

    private static Gesture ParseGesture(string shortcut)
    {
        KeyGesture? gesture;
        try { gesture = new KeyGestureConverter().ConvertFromInvariantString(shortcut) as KeyGesture; }
        catch (Exception ex) when (ex is NotSupportedException or FormatException or ArgumentException)
        {
            throw new ArgumentException("Enter a shortcut such as Ctrl+Alt+Space.", nameof(shortcut), ex);
        }
        if (gesture is null || (gesture.Modifiers & (ModifierKeys.Control | ModifierKeys.Alt)) == 0
            || gesture.Key is Key.None or Key.Escape or Key.LeftCtrl or Key.RightCtrl
                or Key.LeftAlt or Key.RightAlt or Key.LeftShift or Key.RightShift or Key.LWin or Key.RWin)
            throw new ArgumentException("The shortcut must include Ctrl or Alt and a non-Escape, non-modifier key.");
        var key = KeyInterop.VirtualKeyFromKey(gesture.Key);
        if (key == 0) throw new ArgumentException("The shortcut must use a valid key.");
        return new Gesture(key, gesture.Modifiers);
    }

    private sealed record Registration(int Id, Gesture Gesture);

    private readonly record struct Gesture(int Key, ModifierKeys Modifiers)
    {
        public uint NativeModifiers => (Modifiers.HasFlag(ModifierKeys.Alt) ? 1u : 0)
            | (Modifiers.HasFlag(ModifierKeys.Control) ? 2u : 0)
            | (Modifiers.HasFlag(ModifierKeys.Shift) ? 4u : 0)
            | (Modifiers.HasFlag(ModifierKeys.Windows) ? 8u : 0);

        public bool MatchesMessage(IntPtr value) =>
            ((value.ToInt64() >> 16) & 0xFFFF) == Key && (value.ToInt64() & 0xFFFF) == NativeModifiers;
    }

    public void Dispose()
    {
        Configure(false, "");
        EnableCancel(false);
        _releaseTimer.Stop();
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
