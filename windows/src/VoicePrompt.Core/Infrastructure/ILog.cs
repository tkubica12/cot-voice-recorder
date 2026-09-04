using System.Text.RegularExpressions;

namespace VoicePrompt.Core.Infrastructure;

/// <summary>Severity for <see cref="ILog"/> entries.</summary>
public enum LogLevel
{
    Debug,
    Info,
    Warn,
    Error,
}

/// <summary>
/// Minimal logging seam. Callers must only pass operational metadata — never transcript
/// bodies, tokens, client secrets, or Web PubSub access URLs. <see cref="Redactor"/> is a
/// defence-in-depth net for values that slip through.
/// </summary>
public interface ILog
{
    void Write(LogLevel level, string message);
}

public static class LogExtensions
{
    public static void Debug(this ILog log, string message) => log.Write(LogLevel.Debug, message);
    public static void Info(this ILog log, string message) => log.Write(LogLevel.Info, message);
    public static void Warn(this ILog log, string message) => log.Write(LogLevel.Warn, message);
    public static void Error(this ILog log, string message) => log.Write(LogLevel.Error, message);
}

/// <summary>Discards everything. Used by tests and by the safe "unconfigured" CI state.</summary>
public sealed class NullLog : ILog
{
    public static readonly NullLog Instance = new();
    public void Write(LogLevel level, string message) { }
}

/// <summary>
/// Strips credential-shaped substrings from log text: JWTs, <c>access_token=</c> query
/// parameters, and <c>Bearer</c> headers. Applied to every line the file logger writes.
/// </summary>
public static class Redactor
{
    private static readonly Regex Jwt = new(
        @"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private static readonly Regex TokenQuery = new(
        @"(?i)\b(access_token|id_token|refresh_token|code|client_secret)=[^\s&""']+",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private static readonly Regex BearerHeader = new(
        @"(?i)\bBearer\s+[A-Za-z0-9._\-]+",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    public const string Placeholder = "[redacted]";

    public static string Redact(string? text)
    {
        if (string.IsNullOrEmpty(text))
        {
            return string.Empty;
        }

        var s = Jwt.Replace(text, Placeholder);
        s = TokenQuery.Replace(s, m => m.Value[..(m.Value.IndexOf('=') + 1)] + Placeholder);
        s = BearerHeader.Replace(s, "Bearer " + Placeholder);
        return s;
    }
}

/// <summary>
/// Appends redacted lines to a size-capped log file under LocalAppData. Rolls a single
/// <c>.1</c> backup so logs can never grow without bound.
/// </summary>
public sealed class FileLog : ILog
{
    private readonly string _path;
    private readonly IClock _clock;
    private readonly long _maxBytes;
    private readonly LogLevel _minimum;
    private readonly object _gate = new();

    public FileLog(string path, IClock clock, LogLevel minimum = LogLevel.Info, long maxBytes = 512 * 1024)
    {
        _path = path;
        _clock = clock;
        _minimum = minimum;
        _maxBytes = maxBytes;
    }

    public void Write(LogLevel level, string message)
    {
        if (level < _minimum)
        {
            return;
        }

        var line = $"{_clock.UtcNow:yyyy-MM-ddTHH:mm:ss.fffZ} [{level.ToString().ToUpperInvariant()}] "
                   + Redactor.Redact(message);
        lock (_gate)
        {
            try
            {
                var dir = Path.GetDirectoryName(_path);
                if (!string.IsNullOrEmpty(dir))
                {
                    Directory.CreateDirectory(dir);
                }

                if (File.Exists(_path) && new FileInfo(_path).Length > _maxBytes)
                {
                    File.Copy(_path, _path + ".1", overwrite: true);
                    File.Delete(_path);
                }

                File.AppendAllText(_path, line + Environment.NewLine);
            }
            catch
            {
                // Logging must never take the app down.
            }
        }
    }
}
