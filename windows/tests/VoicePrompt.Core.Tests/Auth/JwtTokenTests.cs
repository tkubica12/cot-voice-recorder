using VoicePrompt.Core.Auth;
using Xunit;

namespace VoicePrompt.Core.Tests.Auth;

public class JwtTokenTests
{
    [Fact]
    public void Parse_reads_exp_email_aud_and_sub()
    {
        var exp = DateTimeOffset.UtcNow.AddMinutes(42);
        var jwt = JwtToken.Parse(TestTokens.Jwt(exp, "owner@example.com", "aud-1", "sub-9"));

        Assert.NotNull(jwt.ExpiresAt);
        Assert.Equal(exp.ToUnixTimeSeconds(), jwt.ExpiresAt!.Value.ToUnixTimeSeconds());
        Assert.Equal("owner@example.com", jwt.Email);
        Assert.Equal("aud-1", jwt.Audience);
        Assert.Equal("sub-9", jwt.Subject);
    }

    [Fact]
    public void Parse_reads_the_first_entry_of_an_array_audience()
    {
        const string payload = """{"aud":["first","second"],"exp":2000000000}""";
        var jwt = JwtToken.Parse(
            $"{TestTokens.Base64Url("{}"u8.ToArray())}.{TestTokens.Base64Url(System.Text.Encoding.UTF8.GetBytes(payload))}.");

        Assert.Equal("first", jwt.Audience);
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("not-a-jwt")]
    public void Parse_throws_on_malformed_input(string value) =>
        Assert.Throws<FormatException>(() => JwtToken.Parse(value));

    [Fact]
    public void Parse_throws_when_the_payload_is_not_json() =>
        Assert.ThrowsAny<Exception>(() => JwtToken.Parse("aGVhZGVy.bm90LWpzb24.sig"));

    [Fact]
    public void IsExpiredOrExpiring_is_true_within_the_skew_window()
    {
        var now = DateTimeOffset.UtcNow;
        var token = new JwtToken { ExpiresAt = now.AddMinutes(3) };

        Assert.True(token.IsExpiredOrExpiring(now, TimeSpan.FromMinutes(5)));
        Assert.False(token.IsExpiredOrExpiring(now, TimeSpan.FromMinutes(1)));
    }

    [Fact]
    public void IsExpiredOrExpiring_is_true_for_an_already_expired_token()
    {
        var now = DateTimeOffset.UtcNow;
        Assert.True(new JwtToken { ExpiresAt = now.AddSeconds(-1) }
            .IsExpiredOrExpiring(now, TimeSpan.Zero));
    }

    [Fact]
    public void IsExpiredOrExpiring_treats_a_missing_exp_as_expired()
    {
        var jwt = JwtToken.Parse(TestTokens.Jwt(omitExp: true));

        Assert.Null(jwt.ExpiresAt);
        Assert.True(jwt.IsExpiredOrExpiring(DateTimeOffset.UtcNow, TimeSpan.Zero));
    }

    [Fact]
    public void Base64UrlDecode_handles_all_padding_lengths()
    {
        Assert.Equal(new byte[] { 1 }, JwtToken.Base64UrlDecode("AQ"));
        Assert.Equal(new byte[] { 1, 2 }, JwtToken.Base64UrlDecode("AQI"));
        Assert.Equal(new byte[] { 1, 2, 3 }, JwtToken.Base64UrlDecode("AQID"));
        Assert.Throws<FormatException>(() => JwtToken.Base64UrlDecode("A"));
    }
}
