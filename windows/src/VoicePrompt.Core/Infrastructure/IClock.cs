namespace VoicePrompt.Core.Infrastructure;

/// <summary>Abstraction over the wall clock so time-dependent logic is testable.</summary>
public interface IClock
{
    DateTimeOffset UtcNow { get; }
}

/// <summary>Default system clock.</summary>
public sealed class SystemClock : IClock
{
    public static readonly SystemClock Instance = new();
    public DateTimeOffset UtcNow => DateTimeOffset.UtcNow;
}
