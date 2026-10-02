namespace VoicePrompt.Core.Dictation;

public interface IRecoverableDictationSession : IAsyncDisposable
{
    string Id { get; }
    DictationProgress Progress { get; }
    void Append(ReadOnlySpan<byte> pcm);
    void FinishCapture();
    Task<string?> WaitForCompletionAsync(TimeSpan timeout, CancellationToken ct,
        bool resetTimeoutOnProgress = false);
}
