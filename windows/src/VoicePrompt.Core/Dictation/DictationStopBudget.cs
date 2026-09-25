namespace VoicePrompt.Core.Dictation;

public static class DictationStopBudget
{
    public static TimeSpan Remaining(TimeSpan limit, TimeSpan elapsed, TimeSpan deliveryAllowance)
    {
        if (limit < TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(limit));
        if (elapsed < TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(elapsed));
        if (deliveryAllowance < TimeSpan.Zero) throw new ArgumentOutOfRangeException(nameof(deliveryAllowance));
        var remaining = limit - elapsed - deliveryAllowance;
        return remaining > TimeSpan.Zero ? remaining : TimeSpan.Zero;
    }
}
