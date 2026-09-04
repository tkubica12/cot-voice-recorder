namespace VoicePrompt.Core.Infrastructure;

/// <summary>
/// Minimal file-system seam used by the token store and history cache so that atomic
/// writes and read/delete behavior can be unit-tested with an in-memory fake.
/// </summary>
public interface IFileSystem
{
    bool FileExists(string path);
    string ReadAllText(string path);
    byte[] ReadAllBytes(string path);

    /// <summary>Atomically write <paramref name="bytes"/> to <paramref name="path"/> (temp file + replace).</summary>
    void AtomicWrite(string path, byte[] bytes);

    void AtomicWrite(string path, string text);
    void Delete(string path);
    void CreateDirectory(string path);
}

/// <summary>Real file system with atomic writes via a sibling temp file and move/replace.</summary>
public sealed class PhysicalFileSystem : IFileSystem
{
    public static readonly PhysicalFileSystem Instance = new();

    public bool FileExists(string path) => File.Exists(path);

    public string ReadAllText(string path) => File.ReadAllText(path);

    public byte[] ReadAllBytes(string path) => File.ReadAllBytes(path);

    public void AtomicWrite(string path, byte[] bytes)
    {
        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir))
        {
            Directory.CreateDirectory(dir);
        }

        var tmp = path + ".tmp-" + Guid.NewGuid().ToString("N");
        File.WriteAllBytes(tmp, bytes);
        try
        {
            // File.Move with overwrite is atomic on the same volume.
            File.Move(tmp, path, overwrite: true);
        }
        catch
        {
            SafeDelete(tmp);
            throw;
        }
    }

    public void AtomicWrite(string path, string text) =>
        AtomicWrite(path, System.Text.Encoding.UTF8.GetBytes(text));

    public void Delete(string path) => SafeDelete(path);

    public void CreateDirectory(string path) => Directory.CreateDirectory(path);

    private static void SafeDelete(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch
        {
            // Best effort.
        }
    }
}
