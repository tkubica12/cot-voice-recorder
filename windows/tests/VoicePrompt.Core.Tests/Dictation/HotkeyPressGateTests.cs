using VoicePrompt.Core.Dictation;
using Xunit;

namespace VoicePrompt.Core.Tests.Dictation;

public class HotkeyPressGateTests
{
    [Fact]
    public void Held_shortcut_generates_one_action_despite_repeated_messages()
    {
        var gate = new HotkeyPressGate();
        Assert.True(gate.TryPress());
        for (var repeat = 0; repeat < 500; repeat++)
        {
            Assert.False(gate.ObserveKeyState(true));
            Assert.False(gate.TryPress());
        }
        Assert.True(gate.ObserveKeyState(false));
        Assert.False(gate.ObserveKeyState(false));
        Assert.True(gate.TryPress());
        Assert.False(gate.TryPress());
    }

    [Fact]
    public void Fast_distinct_presses_are_not_time_debounced()
    {
        var gate = new HotkeyPressGate();
        for (var press = 0; press < 20; press++)
        {
            Assert.True(gate.TryPress());
            gate.ObserveKeyState(false);
        }
    }
}
