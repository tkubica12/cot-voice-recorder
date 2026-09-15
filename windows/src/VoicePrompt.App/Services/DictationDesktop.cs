using System.Runtime.InteropServices;
using VoicePrompt.Core.Dictation;

namespace VoicePrompt.App.Services;

public sealed class DictationDesktop : IDictationDesktop
{
    private readonly IntPtr _window = GetForegroundWindow();
    private readonly IntPtr _focus;
    private readonly uint _thread;
    private readonly uint _process;
    private bool _changed;
    private uint _clipboardSequence;

    public DictationDesktop()
    {
        _thread = GetWindowThreadProcessId(_window, out _process);
        _focus = Focus(_thread);
    }

    public bool IsTargetUnchanged
    {
        get
        {
            var thread = GetWindowThreadProcessId(_window, out var process);
            _changed |= _window == IntPtr.Zero || _focus == IntPtr.Zero
                || GetForegroundWindow() != _window || thread != _thread || process != _process
                || Focus(_thread) != _focus;
            return !_changed;
        }
    }

    public bool AreModifiersReleased =>
        new[] { 0x10, 0x11, 0x12, 0x5B, 0x5C }.All(key => (GetAsyncKeyState(key) & 0x8000) == 0);

    public void MarkClipboard() => _clipboardSequence = GetClipboardSequenceNumber();

    public bool TrySendPaste()
    {
        if (!IsTargetUnchanged || !AreModifiersReleased
            || _clipboardSequence == 0 || _clipboardSequence != GetClipboardSequenceNumber())
            return false;
        var input = new[] { Key(0x11, false), Key(0x56, false), Key(0x56, true), Key(0x11, true) };
        var sent = SendInput((uint)input.Length, input, Marshal.SizeOf<Input>());
        if (sent == input.Length) return true;
        if (sent > 0)
        {
            var release = new[] { Key(0x56, true), Key(0x11, true) };
            SendInput((uint)release.Length, release, Marshal.SizeOf<Input>());
        }
        return false;
    }

    private static Input Key(ushort key, bool up) =>
        new() { Type = 1, Data = new InputUnion { Keyboard = new KeyboardInput { VirtualKey = key, Flags = up ? 2u : 0u } } };

    private static IntPtr Focus(uint thread)
    {
        var info = new GuiThreadInfo { Size = (uint)Marshal.SizeOf<GuiThreadInfo>() };
        return GetGUIThreadInfo(thread, ref info) ? info.Focus : IntPtr.Zero;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct GuiThreadInfo
    {
        public uint Size, Flags;
        public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public int Left, Top, Right, Bottom;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct Input { public uint Type; public InputUnion Data; }
    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)] public KeyboardInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public ushort VirtualKey, Scan;
        public uint Flags, Time;
        public UIntPtr ExtraInfo;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int X, Y;
        public uint MouseData, Flags, Time;
        public UIntPtr ExtraInfo;
    }

    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] private static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, Input[] inputs, int size);
}
