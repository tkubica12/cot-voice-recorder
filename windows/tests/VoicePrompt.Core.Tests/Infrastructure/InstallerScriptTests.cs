using System.Text.RegularExpressions;
using Xunit;

namespace VoicePrompt.Core.Tests.Infrastructure;

/// <summary>
/// Source-level validation of the Inno Setup script. Auto-start must be owned by exactly
/// one mechanism — the HKCU <c>...\Run</c> value that the app's Settings toggle reads and
/// writes — so the installer must never also create a Startup-folder shortcut (which the
/// app cannot see and which would launch the tray app twice at sign-in).
/// </summary>
public sealed class InstallerScriptTests
{
    private static string ScriptPath()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            var candidate = Path.Combine(dir.FullName, "windows", "installer", "VoicePrompt.iss");
            if (File.Exists(candidate))
            {
                return candidate;
            }

            dir = dir.Parent;
        }

        throw new FileNotFoundException("Could not locate windows/installer/VoicePrompt.iss");
    }

    private static string Script() => File.ReadAllText(ScriptPath());

    /// <summary>Returns the body of a named Inno Setup section, comments stripped.</summary>
    private static string Section(string name)
    {
        var text = Script();
        var start = text.IndexOf($"[{name}]", StringComparison.OrdinalIgnoreCase);
        if (start < 0)
        {
            return string.Empty;
        }

        start += name.Length + 2;
        var next = Regex.Match(text[start..], @"^\[[A-Za-z]+\]", RegexOptions.Multiline);
        var body = next.Success ? text.Substring(start, next.Index) : text[start..];

        var lines = body.Split('\n')
            .Select(line => line.TrimEnd('\r'))
            .Where(line => !line.TrimStart().StartsWith(";", StringComparison.Ordinal));
        return string.Join("\n", lines);
    }

    [Fact]
    public void Installer_does_not_create_a_startup_folder_shortcut()
    {
        Assert.DoesNotContain("{userstartup}", Section("Icons"), StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Installer_registers_exactly_one_hkcu_run_value()
    {
        var registry = Section("Registry");
        var matches = Regex.Matches(
            registry,
            @"Subkey:\s*""Software\\Microsoft\\Windows\\CurrentVersion\\Run""",
            RegexOptions.IgnoreCase);

        Assert.Single(matches);
        Assert.Contains("ValueName: \"VoicePrompt\"", registry, StringComparison.Ordinal);
        // Removed on uninstall so no orphaned auto-start survives.
        Assert.Contains("uninsdeletevalue", registry, StringComparison.OrdinalIgnoreCase);
        // Seeded only when the user opted in to the startup task.
        Assert.Contains("Tasks: startupicon", registry, StringComparison.Ordinal);
    }

    [Fact]
    public void Run_value_name_matches_the_value_the_app_manages()
    {
        Assert.Contains(
            $"ValueName: \"{VoicePrompt.Core.Tests.Infrastructure.AutoStartContract.ValueName}\"",
            Section("Registry"),
            StringComparison.Ordinal);
    }

    [Fact]
    public void Upgrade_removes_a_legacy_startup_shortcut()
    {
        // Builds up to 1.0.0 shipped a Startup-folder shortcut; upgrades must clean it up
        // or the machine keeps two competing auto-start mechanisms.
        var installDelete = Section("InstallDelete");
        Assert.Contains("{userstartup}", installDelete, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Startup_task_is_declared_once_and_only_drives_the_registry_entry()
    {
        Assert.Single(Regex.Matches(Section("Tasks"), @"Name:\s*""startupicon""", RegexOptions.IgnoreCase));

        var usages = Regex.Matches(Script(), @"Tasks:\s*startupicon", RegexOptions.IgnoreCase);
        Assert.Single(usages);
        Assert.Contains("Tasks: startupicon", Section("Registry"), StringComparison.Ordinal);
    }
}

/// <summary>Mirrors the value name owned by <c>RegistryAutoStartManager</c> in the WPF app.</summary>
internal static class AutoStartContract
{
    // The WPF project is Windows-only and not referenced by these tests, so the contract is
    // asserted against this constant; both sides must stay "VoicePrompt".
    public const string ValueName = "VoicePrompt";
}
