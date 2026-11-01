using System.Globalization;
using VoicePrompt.Core.Dictation;

try
{
    using var input = new MemoryStream();
    Console.OpenStandardInput().CopyTo(input);
    var pcm = input.ToArray();
    using var output = new BinaryWriter(Console.OpenStandardOutput());
    if (args.Length == 1 && args[0] == "wrap")
    {
        output.Write(PcmChunker.ToWav(pcm));
    }
    else if (args.Length is 2 or 3 && args[0] == "chunk")
    {
        var fragment = int.Parse(args[1], CultureInfo.InvariantCulture);
        var boundary = args.Length == 3 ? int.Parse(args[2], CultureInfo.InvariantCulture) : pcm.Length;
        if (fragment <= 0 || boundary < 0 || boundary > pcm.Length)
            throw new ArgumentException("Invalid fragment size or restore boundary.");
        var chunker = new PcmChunker();
        try
        {
            var chunks = new List<AudioChunk>();
            for (var offset = 0; offset < boundary; offset += fragment)
                chunks.AddRange(chunker.Append(pcm.AsSpan(offset, Math.Min(fragment, boundary - offset))));
            if (args.Length == 3)
            {
                var restored = new PcmChunker(chunker.Snapshot());
                chunker.Dispose();
                chunker = restored;
                for (var offset = boundary; offset < pcm.Length; offset += fragment)
                    chunks.AddRange(chunker.Append(pcm.AsSpan(offset, Math.Min(fragment, pcm.Length - offset))));
            }
            chunks.AddRange(chunker.Flush());
            output.Write(chunks.Count);
            foreach (var chunk in chunks)
            {
                output.Write(chunk.EndByte);
                output.Write(chunk.OverlapsPrevious);
                output.Write(chunk.Wav.Length);
                output.Write(chunk.Wav);
            }
        }
        finally { chunker.Dispose(); }
    }
    else
    {
        throw new ArgumentException("Usage: ReferenceAudio wrap | chunk fragment [restore-byte]");
    }
    output.Flush();
    return 0;
}
catch (Exception ex) when (ex is ArgumentException or InvalidDataException or IOException)
{
    Console.Error.WriteLine($"Reference audio probe: {ex.Message}");
    return 2;
}
