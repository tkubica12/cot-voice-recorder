using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;

try
{
    if (!OperatingSystem.IsWindows())
        throw new PlatformNotSupportedException("The independent OS oracle requires Windows.");
    if (args.Length == 2 && args[0] is "instance-hold-fixture" or "instance-quit-fixture")
    {
        if (!Regex.IsMatch(args[1], "^[a-f0-9]{32}$"))
            throw new ArgumentException("Only a fresh synthetic instance key is accepted.");
        var scope = $"Local\\VoicePrompt.RustFixture.{args[1]}";
        using var mutex = new Mutex(true, scope + ".mutex", out var primary);
        using var activate = new EventWaitHandle(false, EventResetMode.AutoReset, scope + ".activate");
        using var quit = new EventWaitHandle(false, EventResetMode.AutoReset, scope + ".quit");
        if (args[0] == "instance-hold-fixture")
        {
            if (!primary) throw new IOException("Synthetic primary already exists.");
            try
            {
                Console.WriteLine("ready");
                Console.Out.Flush();
                if (!quit.WaitOne(TimeSpan.FromSeconds(10)))
                    throw new IOException("Synthetic quit timed out.");
            }
            finally { mutex.ReleaseMutex(); }
        }
        else if (primary)
        {
            mutex.ReleaseMutex();
        }
        else
        {
            quit.Set();
            var acquired = false;
            try { acquired = mutex.WaitOne(TimeSpan.FromSeconds(5)); }
            catch (AbandonedMutexException) { acquired = true; }
            if (!acquired) throw new IOException("Synthetic primary failed to exit.");
            mutex.ReleaseMutex();
        }
        return 0;
    }
    if (args.Length != 1 || args[0] is not ("protect" or "unprotect"))
        throw new ArgumentException("Expected protect, unprotect or an isolated instance fixture.");
    using var input = new MemoryStream();
    Console.OpenStandardInput().CopyTo(input);
    var plain = input.ToArray();
    var entropy = Encoding.UTF8.GetBytes("VoicePrompt.DPAPI.v1");
    try
    {
        var result = args[0] == "protect"
            ? ProtectedData.Protect(plain, entropy, DataProtectionScope.CurrentUser)
            : ProtectedData.Unprotect(plain, entropy, DataProtectionScope.CurrentUser);
        try { Console.OpenStandardOutput().Write(result); }
        finally { CryptographicOperations.ZeroMemory(result); }
    }
    finally { CryptographicOperations.ZeroMemory(plain); }
    return 0;
}
catch (Exception error) when (error is ArgumentException or IOException
    or CryptographicException or PlatformNotSupportedException)
{
    Console.Error.WriteLine($"Independent Windows OS oracle: {error.Message}");
    return 2;
}
