using System.Text;
using VoicePrompt.Core.Auth;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Auth;

public class TokenStoreTests
{
    private const string Path = @"C:\state\tokens.bin";

    private static (TokenStore Store, FakeFileSystem Fs, FakeSecretProtector Protector) Build()
    {
        var fs = new FakeFileSystem();
        var protector = new FakeSecretProtector();
        return (new TokenStore(Path, protector, fs), fs, protector);
    }

    [Fact]
    public void Save_then_Load_round_trips_every_field()
    {
        var (store, _, _) = Build();
        var expires = DateTimeOffset.UtcNow.AddHours(1);

        store.Save(new CachedTokens
        {
            RefreshToken = "refresh-abc",
            IdToken = "id-abc",
            IdTokenExpiresAt = expires,
            Email = "owner@example.com",
        });

        var loaded = store.Load();

        Assert.NotNull(loaded);
        Assert.Equal("refresh-abc", loaded!.RefreshToken);
        Assert.Equal("id-abc", loaded.IdToken);
        Assert.Equal("owner@example.com", loaded.Email);
        Assert.Equal(expires.ToUnixTimeSeconds(), loaded.IdTokenExpiresAt!.Value.ToUnixTimeSeconds());
    }

    [Fact]
    public void Save_encrypts_the_payload_so_no_token_is_readable_at_rest()
    {
        var (store, fs, protector) = Build();

        store.Save(new CachedTokens { RefreshToken = "super-secret-refresh-token" });

        var onDisk = Encoding.UTF8.GetString(fs.ReadAllBytes(Path));
        Assert.Equal(1, protector.ProtectCalls);
        Assert.DoesNotContain("super-secret-refresh-token", onDisk, StringComparison.Ordinal);
        Assert.DoesNotContain("refresh_token", onDisk, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Save_writes_atomically_and_leaves_no_temp_file()
    {
        var (store, fs, _) = Build();

        store.Save(new CachedTokens { RefreshToken = "r" });

        Assert.NotEmpty(fs.ObservedTempFiles);
        Assert.All(fs.ObservedTempFiles, temp => Assert.False(fs.FileExists(temp)));
        Assert.True(fs.FileExists(Path));
    }

    [Fact]
    public void Load_returns_null_when_nothing_is_cached()
    {
        var (store, _, _) = Build();
        Assert.Null(store.Load());
    }

    [Fact]
    public void Load_returns_null_when_the_blob_cannot_be_decrypted()
    {
        var (store, fs, protector) = Build();
        store.Save(new CachedTokens { RefreshToken = "r" });

        // Simulates the file being copied to another user or machine (DPAPI CurrentUser).
        protector.FailUnprotect = true;

        Assert.Null(store.Load());
        Assert.True(fs.FileExists(Path));
    }

    [Fact]
    public void Load_returns_null_for_a_corrupt_or_empty_file()
    {
        var (store, fs, _) = Build();

        fs.Seed(Path, Array.Empty<byte>());
        Assert.Null(store.Load());

        fs.Seed(Path, new byte[] { 0xDE, 0xAD, 0xBE, 0xEF });
        Assert.Null(store.Load());
    }

    [Fact]
    public void Clear_deletes_the_cache_file()
    {
        var (store, fs, _) = Build();
        store.Save(new CachedTokens { RefreshToken = "r" });

        store.Clear();

        Assert.False(fs.FileExists(Path));
        Assert.Null(store.Load());
    }

    [Fact]
    public void Dpapi_protector_round_trips_on_windows()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }

        var protector = new DpapiSecretProtector();
        var plain = Encoding.UTF8.GetBytes("a refresh token");

        var cipher = protector.Protect(plain);

        Assert.NotEqual(plain, cipher);
        Assert.Equal(plain, protector.Unprotect(cipher));
    }
}
