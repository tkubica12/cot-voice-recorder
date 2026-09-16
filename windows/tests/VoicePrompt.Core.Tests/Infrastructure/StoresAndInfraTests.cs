using VoicePrompt.Core.Clipboard;
using VoicePrompt.Core.History;
using VoicePrompt.Core.Infrastructure;
using VoicePrompt.Core.Settings;
using VoicePrompt.Core.Tests.Fakes;
using Xunit;

namespace VoicePrompt.Core.Tests.Infrastructure;

public class HistoryStoreTests
{
    private const string Path = @"C:\state\history.json";

    [Fact]
    public void Original_and_refined_text_round_trip_and_expire_together()
    {
        var (store, fs, clock) = Build();
        store.Add(new HistoryEntry
        {
            TranscriptId = "dictation-a", Body = "Clean text.", RawBody = "Clean clean text.",
            RefinementFallbackBlocks = 2, CompletedAt = clock.UtcNow, CachedAt = clock.UtcNow,
        });
        var reloaded = new HistoryStore(Path, fs, clock);
        Assert.Equal("Clean clean text.", reloaded.Latest()!.RawBody);
        Assert.Equal("Clean text.", reloaded.Latest()!.Body);
        Assert.Equal(2, reloaded.Latest()!.RefinementFallbackBlocks);
        clock.Advance(TimeSpan.FromHours(49));
        Assert.Equal(1, reloaded.Cleanup());
        Assert.DoesNotContain("Clean clean", fs.ReadAllText(Path));
    }

    [Fact]
    public void Removing_cancelled_dictation_keeps_other_history_and_both_versions_are_removed()
    {
        var (store, fs, clock) = Build();
        store.Add(Entry("keep", clock.UtcNow));
        store.Add(new HistoryEntry
        {
            TranscriptId = "cancel", Body = "cleaned", RawBody = "original", CompletedAt = clock.UtcNow,
        });
        var changes = 0;
        store.Changed += () => changes++;
        store.Remove("cancel");
        store.Remove("missing");
        Assert.Equal(1, changes);
        Assert.Equal("keep", new HistoryStore(Path, fs, clock).All().Single().TranscriptId);
        Assert.DoesNotContain("original", fs.ReadAllText(Path));
    }

    [Fact]
    public void Refinement_setting_is_opt_in_and_survives_clone_and_reload()
    {
        var fs = new FakeFileSystem();
        var store = new SettingsStore(@"C:\state\settings.json", fs);
        Assert.False(store.Load().DictationRefinementEnabled);
        fs.Seed(@"C:\state\settings.json", """{"dictation_enabled":true}""");
        Assert.False(store.Load().DictationRefinementEnabled);
        var settings = store.Load();
        settings.DictationRefinementEnabled = true;
        store.Save(settings.Clone());
        Assert.True(store.Load().DictationRefinementEnabled);
    }

    [Fact]
    public void Failed_polish_save_or_cancel_removal_keeps_the_durable_raw_entry_visible()
    {
        var (store, fs, clock) = Build();
        store.Add(new HistoryEntry { TranscriptId = "raw", Body = "Original.", CompletedAt = clock.UtcNow });
        fs.FailNextWrite = new IOException("Injected disk failure.");
        Assert.Throws<IOException>(() => store.Add(new HistoryEntry
        {
            TranscriptId = "raw", Body = "Changed.", RawBody = "Original.", CompletedAt = clock.UtcNow,
        }));
        Assert.Equal("Original.", store.Get("raw")!.Body);
        Assert.Equal("Original.", new HistoryStore(Path, fs, clock).Get("raw")!.Body);
        fs.FailNextWrite = new IOException("Injected disk failure.");
        Assert.Throws<IOException>(() => store.Remove("raw"));
        Assert.NotNull(store.Get("raw"));
        Assert.NotNull(new HistoryStore(Path, fs, clock).Get("raw"));
    }

    private static HistoryEntry Entry(string id, DateTimeOffset completedAt, string body = "body") => new()
    {
        TranscriptId = id,
        RecordingId = "r-" + id,
        Preview = "preview " + id,
        Body = body,
        CompletedAt = completedAt,
        CachedAt = completedAt,
        CharacterCount = body.Length,
    };

    private static (HistoryStore Store, FakeFileSystem Fs, FakeClock Clock) Build(
        int max = 200, TimeSpan? retention = null)
    {
        var fs = new FakeFileSystem();
        var clock = new FakeClock();
        return (new HistoryStore(Path, fs, clock, max, retention), fs, clock);
    }

    [Fact]
    public void Add_then_read_round_trips_through_the_file()
    {
        var (store, fs, clock) = Build();

        store.Add(Entry("a", clock.UtcNow, "the transcript body"));

        var reloaded = new HistoryStore(Path, fs, clock);
        var all = reloaded.All();

        Assert.Single(all);
        Assert.Equal("a", all[0].TranscriptId);
        Assert.Equal("the transcript body", all[0].Body);
        Assert.Equal(19, all[0].CharacterCount);
    }

    [Fact]
    public void Writes_are_atomic_and_leave_no_temp_file()
    {
        var (store, fs, clock) = Build();

        store.Add(Entry("a", clock.UtcNow));

        Assert.NotEmpty(fs.ObservedTempFiles);
        Assert.All(fs.ObservedTempFiles, t => Assert.False(fs.FileExists(t)));
    }

    [Fact]
    public void Entries_are_ordered_newest_first()
    {
        var (store, _, clock) = Build();

        store.Add(Entry("old", clock.UtcNow.AddHours(-5)));
        store.Add(Entry("new", clock.UtcNow));
        store.Add(Entry("mid", clock.UtcNow.AddHours(-1)));

        Assert.Equal(new[] { "new", "mid", "old" }, store.All().Select(e => e.TranscriptId));
        Assert.Equal("new", store.Latest()!.TranscriptId);
    }

    [Fact]
    public void Adding_the_same_transcript_twice_replaces_rather_than_duplicates()
    {
        var (store, _, clock) = Build();

        store.Add(Entry("a", clock.UtcNow, "first"));
        store.Add(Entry("a", clock.UtcNow, "second"));

        Assert.Single(store.All());
        Assert.Equal("second", store.Get("a")!.Body);
    }

    [Fact]
    public void Entries_older_than_48_hours_are_hidden_and_pruned()
    {
        var (store, _, clock) = Build();

        store.Add(Entry("fresh", clock.UtcNow));
        store.Add(Entry("stale", clock.UtcNow.AddHours(-49)));

        Assert.Equal(new[] { "fresh" }, store.All().Select(e => e.TranscriptId));
        Assert.Null(store.Get("stale"));
    }

    [Fact]
    public void Cleanup_removes_expired_entries_and_reports_the_count()
    {
        var (store, _, clock) = Build();
        store.Add(Entry("a", clock.UtcNow));
        store.Add(Entry("b", clock.UtcNow));

        Assert.Equal(0, store.Cleanup());

        clock.Advance(TimeSpan.FromHours(49));

        Assert.Equal(2, store.Cleanup());
        Assert.Empty(store.All());
        Assert.Equal(0, store.Cleanup());
    }

    [Fact]
    public void The_boundary_at_exactly_48_hours_is_expired()
    {
        var (store, _, clock) = Build();
        store.Add(Entry("edge", clock.UtcNow));

        clock.Advance(TimeSpan.FromHours(48));

        Assert.Empty(store.All());
    }

    [Fact]
    public void The_entry_count_is_bounded_keeping_the_newest()
    {
        var (store, _, clock) = Build(max: 3);

        for (var i = 0; i < 10; i++)
        {
            store.Add(Entry($"e{i}", clock.UtcNow.AddMinutes(i)));
        }

        var all = store.All();
        Assert.Equal(3, all.Count);
        Assert.Equal(new[] { "e9", "e8", "e7" }, all.Select(e => e.TranscriptId));
    }

    [Fact]
    public void Clear_removes_the_file_and_all_entries()
    {
        var (store, fs, clock) = Build();
        store.Add(Entry("a", clock.UtcNow));

        store.Clear();

        Assert.Empty(store.All());
        Assert.Null(store.Latest());
        Assert.False(fs.FileExists(Path));
    }

    [Fact]
    public void Changed_fires_on_add_clear_and_pruning_cleanup()
    {
        var (store, _, clock) = Build();
        var changes = 0;
        store.Changed += () => changes++;

        store.Add(Entry("a", clock.UtcNow));
        Assert.Equal(1, changes);

        store.Cleanup();
        Assert.Equal(1, changes);   // nothing pruned

        clock.Advance(TimeSpan.FromHours(49));
        store.Cleanup();
        Assert.Equal(2, changes);

        store.Clear();
        Assert.Equal(3, changes);
    }

    [Fact]
    public void A_corrupt_cache_file_degrades_to_empty_rather_than_throwing()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path, "{ this is not json ]");

        var store = new HistoryStore(Path, fs, new FakeClock());

        Assert.Empty(store.All());
        Assert.Null(store.Latest());
    }

    [Fact]
    public void Entries_without_an_id_are_ignored()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path, """[{"transcript_id":"","body":"x","completed_at":"2026-09-03T12:00:00+00:00"}]""");

        Assert.Empty(new HistoryStore(Path, fs, new FakeClock()).All());
    }

    [Fact]
    public void No_audio_field_is_ever_persisted()
    {
        var (store, fs, clock) = Build();
        store.Add(Entry("a", clock.UtcNow));

        var json = fs.ReadAllText(Path);

        Assert.DoesNotContain("audio", json, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("wav", json, StringComparison.OrdinalIgnoreCase);
    }
}

public class ClipboardCopierTests
{
    [Fact]
    public async Task Copies_on_the_first_attempt()
    {
        var writer = new FakeClipboardWriter();
        var copier = new ClipboardCopier(writer, delay: (_, _) => Task.CompletedTask);

        Assert.Equal(ClipboardCopyResult.Copied, await copier.CopyAsync("hello", CancellationToken.None));
        Assert.Equal(1, writer.Attempts);
        Assert.Equal("hello", writer.LastText);
    }

    [Fact]
    public async Task Retries_a_busy_clipboard_and_then_succeeds()
    {
        var writer = new FakeClipboardWriter { FailuresRemaining = 3 };
        var copier = new ClipboardCopier(writer, maxAttempts: 5, delay: (_, _) => Task.CompletedTask);

        Assert.Equal(ClipboardCopyResult.Copied, await copier.CopyAsync("text", CancellationToken.None));
        Assert.Equal(4, writer.Attempts);
    }

    [Fact]
    public async Task Gives_up_after_the_bounded_number_of_attempts()
    {
        var writer = new FakeClipboardWriter { FailuresRemaining = 99 };
        var copier = new ClipboardCopier(writer, maxAttempts: 4, delay: (_, _) => Task.CompletedTask);

        Assert.Equal(ClipboardCopyResult.Failed, await copier.CopyAsync("text", CancellationToken.None));
        Assert.Equal(4, writer.Attempts);
    }

    [Fact]
    public async Task Empty_text_is_never_written()
    {
        var writer = new FakeClipboardWriter();
        var copier = new ClipboardCopier(writer);

        Assert.Equal(ClipboardCopyResult.Empty, await copier.CopyAsync(null, CancellationToken.None));
        Assert.Equal(ClipboardCopyResult.Empty, await copier.CopyAsync("", CancellationToken.None));
        Assert.Equal(0, writer.Attempts);
    }

    [Fact]
    public async Task Backoff_grows_between_attempts()
    {
        var delays = new List<TimeSpan>();
        var writer = new FakeClipboardWriter { FailuresRemaining = 3 };
        var copier = new ClipboardCopier(writer, maxAttempts: 5, delay: (d, _) => { delays.Add(d); return Task.CompletedTask; });

        await copier.CopyAsync("t", CancellationToken.None);

        Assert.Equal(3, delays.Count);
        Assert.True(delays[0] < delays[2]);
        Assert.All(delays, d => Assert.True(d <= TimeSpan.FromMilliseconds(500)));
    }

    [Fact]
    public async Task Cancellation_is_propagated()
    {
        using var cts = new CancellationTokenSource();
        await cts.CancelAsync();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => new ClipboardCopier(new FakeClipboardWriter()).CopyAsync("t", cts.Token));
    }

    [Fact]
    public void Rejects_a_non_positive_attempt_bound() =>
        Assert.Throws<ArgumentOutOfRangeException>(() => new ClipboardCopier(new FakeClipboardWriter(), 0));
}

public class SettingsStoreTests
{
    private const string Path = @"C:\state\settings.json";

    [Fact]
    public void Defaults_are_returned_when_no_file_exists()
    {
        var settings = new SettingsStore(Path, new FakeFileSystem()).Load();

        Assert.Equal(AppSettings.DefaultBackendBaseUrl, settings.BackendBaseUrl);
        Assert.False(settings.NotificationsPaused);
        Assert.False(settings.AutoStart);
    }

    [Fact]
    public void Save_then_Load_round_trips()
    {
        var fs = new FakeFileSystem();
        var store = new SettingsStore(Path, fs);

        store.Save(new AppSettings
        {
            BackendBaseUrl = "https://api.example.test",
            NotificationsPaused = true,
            AutoStart = true,
        });

        var loaded = new SettingsStore(Path, fs).Load();

        Assert.Equal("https://api.example.test", loaded.BackendBaseUrl);
        Assert.True(loaded.NotificationsPaused);
        Assert.True(loaded.AutoStart);
    }

    [Fact]
    public void A_corrupt_settings_file_falls_back_to_defaults()
    {
        var fs = new FakeFileSystem();
        fs.Seed(Path, "not json");

        Assert.Equal(AppSettings.DefaultBackendBaseUrl, new SettingsStore(Path, fs).Load().BackendBaseUrl);
    }

    [Theory]
    [InlineData("", AppSettings.DefaultBackendBaseUrl)]
    [InlineData("   ", AppSettings.DefaultBackendBaseUrl)]
    [InlineData("not a url", AppSettings.DefaultBackendBaseUrl)]
    [InlineData("ftp://example.test", AppSettings.DefaultBackendBaseUrl)]
    [InlineData("https://api.example.test/", "https://api.example.test")]
    [InlineData("  https://api.example.test  ", "https://api.example.test")]
    [InlineData("http://localhost:8000", "http://localhost:8000")]
    public void Backend_urls_are_normalised_and_validated(string? input, string expected) =>
        Assert.Equal(expected, AppSettings.NormalizeBaseUrl(input));

    [Fact]
    public void Settings_never_contain_secret_material()
    {
        var fs = new FakeFileSystem();
        new SettingsStore(Path, fs).Save(new AppSettings());

        var json = fs.ReadAllText(Path);

        Assert.DoesNotContain("token", json, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("secret", json, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("client_id", json, StringComparison.OrdinalIgnoreCase);
    }
}

public class AppPathsTests
{
    [Fact]
    public void All_state_lives_under_a_single_root()
    {
        var paths = new AppPaths(@"C:\Users\me\AppData\Local\VoicePrompt");

        Assert.StartsWith(paths.Root, paths.TokensFile);
        Assert.StartsWith(paths.Root, paths.HistoryFile);
        Assert.StartsWith(paths.Root, paths.SettingsFile);
        Assert.StartsWith(paths.Root, paths.LogFile);
        Assert.EndsWith("tokens.bin", paths.TokensFile);
        Assert.EndsWith("history.json", paths.HistoryFile);
        Assert.EndsWith("settings.json", paths.SettingsFile);
    }

    [Fact]
    public void Default_is_under_LocalApplicationData()
    {
        var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        Assert.Equal(System.IO.Path.Combine(local, "VoicePrompt"), AppPaths.Default().Root);
    }

    [Fact]
    public void EnsureCreated_creates_the_root_and_log_directories()
    {
        var fs = new FakeFileSystem();
        var paths = new AppPaths(@"C:\state");

        paths.EnsureCreated(fs);

        Assert.Contains(@"C:\state", fs.Directories);
        Assert.Contains(paths.LogDirectory, fs.Directories);
    }
}

public class RedactorTests
{
    [Fact]
    public void Redacts_a_jwt_anywhere_in_the_line()
    {
        var jwt = TestTokens.Jwt();
        var text = Redactor.Redact($"sending token {jwt} to the backend");

        Assert.DoesNotContain(jwt, text);
        Assert.Contains(Redactor.Placeholder, text);
        Assert.Contains("to the backend", text);
    }

    [Theory]
    [InlineData("wss://x/client?access_token=abc.def.ghi", "abc.def.ghi")]
    [InlineData("?id_token=zzz&x=1", "zzz")]
    [InlineData("refresh_token=rrr", "rrr")]
    [InlineData("client_secret=sss", "sss")]
    [InlineData("code=cccccc", "cccccc")]
    public void Redacts_credential_query_parameters(string input, string secret)
    {
        var redacted = Redactor.Redact(input);

        Assert.DoesNotContain(secret, redacted);
        Assert.Contains(Redactor.Placeholder, redacted);
    }

    [Fact]
    public void Redacts_a_bearer_header()
    {
        var redacted = Redactor.Redact("Authorization: Bearer abc.def.ghi");

        Assert.DoesNotContain("abc.def.ghi", redacted);
        Assert.Contains("Bearer " + Redactor.Placeholder, redacted);
    }

    [Fact]
    public void Leaves_ordinary_text_alone()
    {
        Assert.Equal("realtime: connected", Redactor.Redact("realtime: connected"));
        Assert.Equal(string.Empty, Redactor.Redact(null));
    }

    [Fact]
    public void FileLog_writes_redacted_lines_only()
    {
        var dir = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "vp-log-" + Guid.NewGuid().ToString("N"));
        var path = System.IO.Path.Combine(dir, "app.log");
        try
        {
            var log = new FileLog(path, new FakeClock(), LogLevel.Debug);
            var jwt = TestTokens.Jwt();

            log.Info($"token was {jwt}");
            log.Debug("plain line");

            var content = File.ReadAllText(path);
            Assert.DoesNotContain(jwt, content);
            Assert.Contains("plain line", content);
            Assert.Contains("[INFO]", content);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    [Fact]
    public void FileLog_honours_the_minimum_level()
    {
        var dir = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "vp-log-" + Guid.NewGuid().ToString("N"));
        var path = System.IO.Path.Combine(dir, "app.log");
        try
        {
            var log = new FileLog(path, new FakeClock(), LogLevel.Warn);
            log.Debug("should not appear");
            log.Warn("should appear");

            var content = File.ReadAllText(path);
            Assert.DoesNotContain("should not appear", content);
            Assert.Contains("should appear", content);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }
}

public class PhysicalFileSystemTests
{
    [Fact]
    public void AtomicWrite_replaces_content_and_removes_the_temp_file()
    {
        var dir = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "vp-fs-" + Guid.NewGuid().ToString("N"));
        var path = System.IO.Path.Combine(dir, "data.json");
        try
        {
            var fs = PhysicalFileSystem.Instance;

            fs.AtomicWrite(path, "first");
            Assert.Equal("first", fs.ReadAllText(path));

            fs.AtomicWrite(path, "second");
            Assert.Equal("second", fs.ReadAllText(path));

            Assert.Single(Directory.GetFiles(dir));

            fs.Delete(path);
            Assert.False(fs.FileExists(path));
            // Deleting a missing file is a no-op.
            fs.Delete(path);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }
}

public class SingleInstanceGuardTests
{
    [Fact]
    public void The_first_holder_is_primary_and_a_second_is_not()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");

        using var first = SingleInstanceGuard.Acquire(name);
        Assert.True(first.IsPrimary);

        using var second = SingleInstanceGuard.Acquire(name);
        Assert.False(second.IsPrimary);
    }

    [Fact]
    public void The_guard_is_released_on_dispose()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");

        using (var first = SingleInstanceGuard.Acquire(name))
        {
            Assert.True(first.IsPrimary);
        }

        using var next = SingleInstanceGuard.Acquire(name);
        Assert.True(next.IsPrimary);
    }

    [Fact]
    public void Different_names_do_not_collide()
    {
        using var a = SingleInstanceGuard.Acquire("VoicePrompt.Tests.A." + Guid.NewGuid().ToString("N"));
        using var b = SingleInstanceGuard.Acquire("VoicePrompt.Tests.B." + Guid.NewGuid().ToString("N"));

        Assert.True(a.IsPrimary);
        Assert.True(b.IsPrimary);
    }

    [Fact]
    public void A_secondary_instance_can_signal_the_primary_to_activate()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");
        using var primary = SingleInstanceGuard.Acquire(name);

        using var activated = new ManualResetEventSlim(false);
        primary.ListenForActivation(() => activated.Set());

        using (var secondary = SingleInstanceGuard.Acquire(name))
        {
            Assert.False(secondary.IsPrimary);
            secondary.SignalExistingInstance();
        }

        Assert.True(activated.Wait(TimeSpan.FromSeconds(5)));
    }

    [Fact]
    public void A_secondary_instance_can_ask_the_primary_to_quit()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");
        using var primary = SingleInstanceGuard.Acquire(name);

        using var activated = new ManualResetEventSlim(false);
        using var quitting = new ManualResetEventSlim(false);
        primary.ListenForActivation(() => activated.Set(), () => quitting.Set());

        using (var secondary = SingleInstanceGuard.Acquire(name))
        {
            secondary.SignalQuit();
        }

        Assert.True(quitting.Wait(TimeSpan.FromSeconds(5)));
        Assert.False(activated.IsSet);
    }

    [Fact]
    public void WaitForPrimaryExit_returns_once_the_primary_releases_the_guard()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");
        var primary = SingleInstanceGuard.Acquire(name);
        using var secondary = SingleInstanceGuard.Acquire(name);

        // A mutex is owned per thread, so the wait must run off the owning thread to model
        // the real cross-process case.
        static bool WaitOffThread(SingleInstanceGuard guard, TimeSpan timeout)
        {
            var result = false;
            var thread = new Thread(() => result = guard.WaitForPrimaryExit(timeout));
            thread.Start();
            thread.Join();
            return result;
        }

        Assert.False(WaitOffThread(secondary, TimeSpan.FromMilliseconds(100)));

        primary.Dispose();

        Assert.True(WaitOffThread(secondary, TimeSpan.FromSeconds(5)));
    }

    [Fact]
    public void WaitForPrimaryExit_is_immediate_for_the_primary_itself()
    {
        using var primary = SingleInstanceGuard.Acquire("VoicePrompt.Tests." + Guid.NewGuid().ToString("N"));
        Assert.True(primary.WaitForPrimaryExit(TimeSpan.Zero));
    }

    [Fact]
    public void A_non_primary_instance_never_listens()
    {
        var name = "VoicePrompt.Tests." + Guid.NewGuid().ToString("N");
        using var primary = SingleInstanceGuard.Acquire(name);
        using var secondary = SingleInstanceGuard.Acquire(name);

        var called = false;
        secondary.ListenForActivation(() => called = true);
        secondary.SignalExistingInstance();

        Thread.Sleep(150);
        Assert.False(called);
    }
}

public class AutoStartTests
{
    [Fact]
    public void The_null_manager_records_intent()
    {
        var manager = new NullAutoStartManager();

        Assert.False(manager.IsEnabled);
        Assert.True(manager.Set(true));
        Assert.True(manager.IsEnabled);
        Assert.True(manager.Set(false));
        Assert.False(manager.IsEnabled);
    }
}
