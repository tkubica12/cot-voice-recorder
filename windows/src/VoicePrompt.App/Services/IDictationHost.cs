using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Settings;

namespace VoicePrompt.App.Services;

internal interface IDictationHost
{
    AppSettings Settings { get; }
    bool CanDictate { get; }
    bool DictationBusy { get; set; }
    HistoryStore History { get; }
    IClock Clock { get; }
    ILog Log { get; }
    Task<string> TranscribeAsync(byte[] wav, string language, CancellationToken ct);
}
