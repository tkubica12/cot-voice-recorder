using System.Buffers.Binary;

namespace VoicePrompt.Core.Dictation;

public sealed record AudioChunk(byte[] Wav, bool OverlapsPrevious);

/// <summary>16 kHz mono PCM: cut at pauses, with bounded overlapping chunks during continuous speech.</summary>
public sealed class PcmChunker
{
    public const int SampleRate = 16000;
    public const int BytesPerSecond = SampleRate * 2;
    public const int MaxChunkMilliseconds = 6000;
    public const int OverlapMilliseconds = 600;
    private const int FrameBytes = BytesPerSecond / 50; // 20 ms
    private readonly byte[] _audio = new byte[BytesPerSecond * MaxChunkMilliseconds / 1000];
    private readonly byte[] _frame = new byte[FrameBytes];
    private readonly double _silenceRms;
    private int _frameLength;
    private int _length;
    private int _silentFrames;
    private int _newBytes;
    private bool _hasSpeech;
    private bool _overlaps;

    public PcmChunker(double silenceRms = 120)
    {
        if (!double.IsFinite(silenceRms) || silenceRms < 0)
            throw new ArgumentOutOfRangeException(nameof(silenceRms));
        _silenceRms = silenceRms;
    }

    public double Level { get; private set; }

    public IReadOnlyList<AudioChunk> Append(ReadOnlySpan<byte> pcm)
    {
        var chunks = new List<AudioChunk>();
        while (!pcm.IsEmpty)
        {
            var count = Math.Min(FrameBytes - _frameLength, pcm.Length);
            pcm[..count].CopyTo(_frame.AsSpan(_frameLength));
            _frameLength += count;
            pcm = pcm[count..];
            if (_frameLength == FrameBytes)
            {
                ProcessFrame(_frame, chunks);
                _frameLength = 0;
            }
        }
        return chunks;
    }

    public IReadOnlyList<AudioChunk> Flush()
    {
        if (_frameLength % 2 != 0)
            throw new InvalidDataException("Incomplete PCM sample.");
        var chunks = new List<AudioChunk>();
        if (_frameLength > 0)
        {
            ProcessFrame(_frame.AsSpan(0, _frameLength), chunks);
            _frameLength = 0;
        }
        if (_hasSpeech && _newBytes > 0)
            chunks.Add(Emit(false));
        Clear();
        return chunks;
    }

    private void ProcessFrame(ReadOnlySpan<byte> frame, List<AudioChunk> chunks)
    {
        double energy = 0;
        for (var i = 0; i < frame.Length; i += 2)
        {
            var value = BinaryPrimitives.ReadInt16LittleEndian(frame[i..]);
            energy += (double)value * value;
        }
        var rms = Math.Sqrt(energy / (frame.Length / 2));
        Level = Math.Clamp(rms / 6000, 0, 1);
        var voiced = rms >= _silenceRms;
        _hasSpeech |= voiced;
        _silentFrames = voiced ? 0 : _silentFrames + 1;
        frame.CopyTo(_audio.AsSpan(_length));
        _length += frame.Length;
        _newBytes += frame.Length;

        if (!_hasSpeech && _overlaps && _silentFrames >= 20)
        {
            _length = _newBytes = 0;
            _overlaps = false;
        }
        else if (!_hasSpeech && !_overlaps && _length >= BytesPerSecond / 5)
        {
            // Keep 180 ms of pre-roll rather than sending long silent requests.
            _audio.AsSpan(FrameBytes, _length - FrameBytes).CopyTo(_audio);
            _length -= FrameBytes;
            _newBytes = _length;
        }
        else if (_hasSpeech && _length >= BytesPerSecond * 2 && _silentFrames >= 20)
        {
            chunks.Add(Emit(false));
        }
        else if (_length == _audio.Length)
        {
            chunks.Add(Emit(true));
        }
    }

    private AudioChunk Emit(bool keepOverlap)
    {
        var result = new AudioChunk(ToWav(_audio.AsSpan(0, _length)), _overlaps);
        var overlap = keepOverlap ? BytesPerSecond * OverlapMilliseconds / 1000 : 0;
        if (overlap > 0)
            _audio.AsSpan(_length - overlap, overlap).CopyTo(_audio);
        _length = overlap;
        _newBytes = 0;
        _overlaps = keepOverlap;
        _hasSpeech = false;
        _silentFrames = 0;
        return result;
    }

    private void Clear()
    {
        Array.Clear(_audio);
        Array.Clear(_frame);
        _length = _newBytes = _frameLength = _silentFrames = 0;
        _hasSpeech = _overlaps = false;
    }

    public static byte[] ToWav(ReadOnlySpan<byte> pcm)
    {
        if (pcm.Length == 0 || pcm.Length % 2 != 0)
            throw new ArgumentException("PCM must contain complete 16-bit samples.", nameof(pcm));
        var wav = new byte[44 + pcm.Length];
        "RIFF"u8.CopyTo(wav);
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(4), wav.Length - 8);
        "WAVEfmt "u8.CopyTo(wav.AsSpan(8));
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(16), 16);
        BinaryPrimitives.WriteInt16LittleEndian(wav.AsSpan(20), 1);
        BinaryPrimitives.WriteInt16LittleEndian(wav.AsSpan(22), 1);
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(24), SampleRate);
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(28), BytesPerSecond);
        BinaryPrimitives.WriteInt16LittleEndian(wav.AsSpan(32), 2);
        BinaryPrimitives.WriteInt16LittleEndian(wav.AsSpan(34), 16);
        "data"u8.CopyTo(wav.AsSpan(36));
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(40), pcm.Length);
        pcm.CopyTo(wav.AsSpan(44));
        return wav;
    }
}
