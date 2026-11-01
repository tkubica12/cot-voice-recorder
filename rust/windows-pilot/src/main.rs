#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use voiceprompt_windows_pilot::{
        ClientOptions, TrayOptions, protect, request_quit, run_tray, unprotect,
    };

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 2
        && matches!(
            args[0].to_str(),
            Some("--instance-hold-fixture" | "--instance-quit-fixture")
        )
    {
        use voiceprompt_windows_pilot::{CompatibleInstance, InstanceEvent};
        let key = args[1].to_str().ok_or("Invalid fixture instance key.")?;
        voiceprompt_windows_pilot::validate_instance_key(key)?;
        let instance = CompatibleInstance::acquire(&format!("VoicePrompt.RustFixture.{key}"))?;
        if args[0] == "--instance-quit-fixture" {
            instance.request_quit()?;
            instance.wait_for_exit(std::time::Duration::from_secs(5))?;
            return Ok(());
        }
        if !instance.primary {
            return Err("Fixture instance already exists.".into());
        }
        std::io::stdout().lock().write_all(b"ready\n")?;
        std::io::stdout().lock().flush()?;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < until {
            if matches!(instance.poll()?, Some(InstanceEvent::Quit)) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        return Err("Fixture primary did not receive a bounded quit.".into());
    }
    if args.len() == 3 && args[0] == "--recovery-delete-idle-fixture" {
        use voiceprompt_windows_pilot::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
        let store = NativeRecoveryStore::new(PathBuf::from(&args[1]), DEFAULT_RECOVERY_QUOTA)?;
        store.delete_idle(args[2].to_str().ok_or("Invalid recovery ID encoding.")?)?;
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--recovery-root-lock-fixture" {
        use voiceprompt_windows_pilot::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
        let _owner = NativeRecoveryStore::new(PathBuf::from(&args[1]), DEFAULT_RECOVERY_QUOTA)?;
        std::io::stdout().lock().write_all(b"ready\n")?;
        std::io::stdout().lock().flush()?;
        let (sent, released) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut signal = [0; 1];
            let result = std::io::stdin().lock().read(&mut signal);
            let _ = sent.send(result);
        });
        released.recv_timeout(std::time::Duration::from_secs(10))??;
        return Ok(());
    }
    if args.len() == 3 && args[0] == "--recovery-open-journal-fixture" {
        use voiceprompt_windows_pilot::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
        let store = NativeRecoveryStore::new(PathBuf::from(&args[1]), DEFAULT_RECOVERY_QUOTA)?;
        let journal = store.open(args[2].to_str().ok_or("Invalid recovery ID encoding.")?)?;
        let mut plain = journal.snapshot()?.encode()?;
        let written = std::io::stdout().lock().write_all(&plain);
        plain.fill(0);
        written?;
        return Ok(());
    }
    if args.len() == 3 && args[0] == "--recovery-journal-fixture" {
        use voiceprompt_client_protocol::recovery::{AudioCheckpoint, MAX_AUDIO_BYTES};
        use voiceprompt_windows_pilot::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
        let mode = args[2].to_str().ok_or("Invalid recovery fixture mode.")?;
        if !matches!(mode, "legacy" | "streaming" | "audit") {
            return Err("Unknown recovery fixture mode.".into());
        }
        let mut wav = Vec::new();
        std::io::stdin()
            .lock()
            .take((MAX_AUDIO_BYTES + 1) as u64)
            .read_to_end(&mut wav)?;
        let pcm_bytes = wav
            .len()
            .checked_sub(44)
            .ok_or("Invalid recovery fixture WAV.")? as u64;
        let store = NativeRecoveryStore::new(PathBuf::from(&args[1]), DEFAULT_RECOVERY_QUOTA)?;
        let now = time::OffsetDateTime::now_utc();
        let journal = store.create(
            "auto".into(),
            "synthetic context".into(),
            mode != "legacy",
            now,
        )?;
        let saved = journal.checkpoint(
            &[],
            pcm_bytes * 2,
            &[
                AudioCheckpoint {
                    wav: &wav,
                    overlaps_previous: false,
                    end_byte: pcm_bytes,
                },
                AudioCheckpoint {
                    wav: &wav,
                    overlaps_previous: false,
                    end_byte: pcm_bytes * 2,
                },
            ],
            true,
            now,
        );
        wav.fill(0);
        saved?;
        if mode == "legacy" {
            journal.save_result(0, "synthetic first".into(), now)?;
        } else {
            journal.confirm_streaming(
                pcm_bytes,
                "synthetic first".into(),
                vec![0; 100],
                false,
                None,
                now,
            )?;
            if mode == "audit" {
                journal.confirm_streaming(
                    pcm_bytes * 2,
                    "synthetic first final".into(),
                    vec![0; 100],
                    false,
                    None,
                    now,
                )?;
                journal.prepare_audit("synthetic polished final".into(), now, true, now)?;
            }
        }
        let mut plain = journal.snapshot()?.encode()?;
        let written = std::io::stdout().lock().write_all(&plain);
        plain.fill(0);
        written?;
        return Ok(());
    }
    if args.len() == 2
        && matches!(
            args[0].to_str(),
            Some("--recovery-save-fixture" | "--recovery-load-fixture")
        )
    {
        use voiceprompt_client_protocol::recovery::MAX_MANIFEST_BYTES;
        use voiceprompt_windows_pilot::NativeRecoveryManifestStore;
        let store = NativeRecoveryManifestStore::new(PathBuf::from(&args[1]))?;
        if args[0] == "--recovery-save-fixture" {
            let mut data = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_MANIFEST_BYTES + 1) as u64)
                .read_to_end(&mut data)?;
            let saved = store.save_fixture(&data);
            data.fill(0);
            saved?;
        } else {
            let mut data = store.load()?.encode()?;
            let written = std::io::stdout().lock().write_all(&data);
            data.fill(0);
            written?;
        }
        return Ok(());
    }
    if args.len() == 2
        && matches!(
            args[0].to_str(),
            Some("--settings-save" | "--settings-load")
        )
    {
        use voiceprompt_client_protocol::{MAX_AUTH_JSON_BYTES, settings::AppSettings};
        use voiceprompt_windows_pilot::NativeSettingsStore;
        let store = NativeSettingsStore::new(PathBuf::from(&args[1]))?;
        if args[0] == "--settings-save" {
            let mut data = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_AUTH_JSON_BYTES + 1) as u64)
                .read_to_end(&mut data)?;
            store.save(&AppSettings::parse(&data)?)?;
        } else {
            std::io::stdout()
                .lock()
                .write_all(&store.load()?.encode()?)?;
        }
        return Ok(());
    }
    if args.len() == 2 && matches!(args[0].to_str(), Some("--history-add" | "--history-load")) {
        use voiceprompt_client_protocol::history::{
            MAX_HISTORY_BYTES, encode_history, parse_history,
        };
        use voiceprompt_windows_pilot::NativeHistoryStore;
        let store = NativeHistoryStore::new(PathBuf::from(&args[1]))?;
        let now = time::OffsetDateTime::now_utc();
        if args[0] == "--history-add" {
            let mut data = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_HISTORY_BYTES + 1) as u64)
                .read_to_end(&mut data)?;
            let mut entries = parse_history(&data)?;
            if entries.len() != 1 {
                return Err("History fixture must contain exactly one entry.".into());
            }
            store.add(entries.remove(0), now)?;
        } else {
            std::io::stdout()
                .lock()
                .write_all(&encode_history(&store.all(now)?)?)?;
        }
        return Ok(());
    }
    if args.len() == 2
        && matches!(
            args[0].to_str(),
            Some("--cache-save" | "--cache-load" | "--cache-clear")
        )
    {
        use voiceprompt_client_protocol::auth::CachedTokens;
        use voiceprompt_windows_pilot::EncryptedTokenStore;
        let store = EncryptedTokenStore::new(PathBuf::from(&args[1]))?;
        match args[0].to_str() {
            Some("--cache-save") => {
                let mut data = Vec::new();
                std::io::stdin()
                    .lock()
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut data)?;
                let parsed = CachedTokens::parse(&data)?;
                data.fill(0);
                store.save(&parsed)?;
            }
            Some("--cache-load") => {
                let mut data = match store.load()? {
                    Some(tokens) => tokens.encode()?,
                    None => b"null".to_vec(),
                };
                std::io::stdout().lock().write_all(&data)?;
                data.fill(0);
            }
            Some("--cache-clear") => store.clear()?,
            _ => unreachable!(),
        }
        return Ok(());
    }
    if args.len() == 1 && (args[0] == "--protect-stdin" || args[0] == "--unprotect-stdin") {
        let mut input = Vec::new();
        std::io::stdin()
            .lock()
            .take(16 * 1024 * 1024 + 64 * 1024 + 1)
            .read_to_end(&mut input)?;
        let output = if args[0] == "--protect-stdin" {
            protect(&input)?
        } else {
            unprotect(&input)?
        };
        std::io::stdout().lock().write_all(&output)?;
        return Ok(());
    }
    let mut options = TrayOptions {
        instance_key: "interactive".into(),
        ready_file: None,
        run_for_seconds: None,
        self_test: false,
        client: None,
    };
    let mut state_directory = None;
    let mut oauth_config = None;
    let mut installed = std::env::current_exe()?
        .file_name()
        .is_some_and(|name| name == "VoicePrompt.exe");
    let mut quit = false;
    let mut position = 0;
    while position < args.len() {
        match args[position].to_str() {
            Some("--self-test") => options.self_test = true,
            Some("--quit") => quit = true,
            Some("--installed") => installed = true,
            Some("--instance-key" | "--ready-file" | "--run-for-seconds" | "--state-directory" | "--oauth-config") => {
                let option = args[position].to_str().ok_or("Invalid option encoding.")?;
                position += 1;
                let value = args.get(position).ok_or("Missing option value.")?;
                match option {
                    "--instance-key" => options.instance_key = value.to_str().ok_or("Instance key must be ASCII.")?.into(),
                    "--ready-file" => options.ready_file = Some(PathBuf::from(value)),
                    "--run-for-seconds" => options.run_for_seconds = Some(value.to_str().ok_or("Invalid duration encoding.")?.parse()?),
                    "--state-directory" => state_directory = Some(PathBuf::from(value)),
                    "--oauth-config" => oauth_config = Some(PathBuf::from(value)),
                    _ => unreachable!(),
                }
                if installed {
                    if options.self_test || options.ready_file.is_some() || state_directory.is_some() || oauth_config.is_some() {
                        return Err("Installed mode cannot be combined with fixture/benchmark/explicit-state startup.".into());
                    }
                    if quit {
                        let instance = voiceprompt_windows_pilot::CompatibleInstance::acquire("VoicePrompt.SingleInstance")?;
                        if !instance.primary {
                            instance.request_quit()?;
                            instance.wait_for_exit(std::time::Duration::from_secs(20))?;
                        }
                        return Ok(());
                    }
                    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable.")?;
                    let path = PathBuf::from(local).join("VoicePrompt");
                    std::fs::create_dir_all(&path)?;
                    let configured = std::env::var_os("VOICEPROMPT_GOOGLE_DESKTOP_CLIENT").map(PathBuf::from);
                    let beside = std::env::current_exe()?.parent().ok_or("Missing executable directory.")?.join("google-desktop-client.json");
                    if let Some(configured) = configured { oauth_config = Some(configured); }
                    else {
                        for candidate in [path.join("google-desktop-client.json"), beside] {
                            if candidate.try_exists()? { oauth_config = Some(candidate); break; }
                        }
                    }
                    state_directory = Some(path);
                }
            }
            _ => return Err("Usage: VoicePromptRustPilot [--self-test] [--instance-key key] [--ready-file path] [--run-for-seconds 1-3600] [--state-directory absolute-path [--oauth-config path]] | --quit --instance-key key | explicit-path cache/history/settings fixture commands".into()),
        }
        position += 1;
    }
    if oauth_config.is_some() && state_directory.is_none() {
        return Err("--oauth-config requires an explicit --state-directory.".into());
    }
    if let Some(state_directory) = state_directory {
        if options.ready_file.is_some() {
            return Err(
                "Foundation measurement readiness cannot be combined with connected mode.".into(),
            );
        }
        options.client = Some(ClientOptions {
            state_directory,
            oauth_config,
            clipboard_enabled: !options.self_test,
            installed,
        });
    }
    if quit {
        if options.ready_file.is_some()
            || options.run_for_seconds.is_some()
            || options.self_test
            || options.client.is_some()
        {
            return Err("--quit cannot be combined with startup options.".into());
        }
        request_quit(&options.instance_key)?;
    } else {
        run_tray(options)?;
    }
    Ok(())
}

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("VoicePrompt Rust pilot: {error}");
        let automated = std::env::args_os().skip(1).any(|argument| {
            matches!(
                argument.to_str(),
                Some(
                    "--self-test"
                        | "--run-for-seconds"
                        | "--ready-file"
                        | "--settings-save"
                        | "--settings-load"
                        | "--cache-save"
                        | "--cache-load"
                        | "--cache-clear"
                        | "--history-add"
                        | "--history-load"
                        | "--protect-stdin"
                        | "--unprotect-stdin"
                        | "--quit"
                        | "--recovery-save-fixture"
                        | "--recovery-load-fixture"
                        | "--recovery-root-lock-fixture"
                        | "--recovery-journal-fixture"
                        | "--recovery-open-journal-fixture"
                        | "--recovery-delete-idle-fixture"
                        | "--instance-hold-fixture"
                        | "--instance-quit-fixture"
                )
            )
        });
        if !automated {
            use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
            use windows::core::w;
            unsafe {
                let _ = MessageBoxW(
                    None,
                    w!(
                        "The isolated Rust client could not start. Check its explicit configuration and state paths. Existing files have been preserved; diagnostics are written to stderr."
                    ),
                    w!("VoicePrompt Rust client"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(2);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("VoicePromptRustPilot requires Windows. The audio core remains platform-neutral.");
    std::process::exit(2);
}
