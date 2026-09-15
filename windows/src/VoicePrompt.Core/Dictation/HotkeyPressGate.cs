namespace VoicePrompt.Core.Dictation;

public sealed class HotkeyPressGate
{
    private bool _pressed;

    public bool TryPress()
    {
        if (_pressed) return false;
        _pressed = true;
        return true;
    }

    public bool ObserveKeyState(bool isDown)
    {
        if (isDown || !_pressed) return false;
        _pressed = false;
        return true;
    }
}
