#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use voiceprompt_windows_pilot::{TrayOptions, protect, request_quit, run_tray, unprotect};

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
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
    };
    let mut quit = false;
    let mut position = 0;
    while position < args.len() {
        match args[position].to_str() {
            Some("--self-test") => options.self_test = true,
            Some("--quit") => quit = true,
            Some("--instance-key" | "--ready-file" | "--run-for-seconds") => {
                let option = args[position].to_str().ok_or("Invalid option encoding.")?;
                position += 1;
                let value = args.get(position).ok_or("Missing option value.")?;
                match option {
                    "--instance-key" => options.instance_key = value.to_str().ok_or("Instance key must be ASCII.")?.into(),
                    "--ready-file" => options.ready_file = Some(PathBuf::from(value)),
                    "--run-for-seconds" => options.run_for_seconds = Some(value.to_str().ok_or("Invalid duration encoding.")?.parse()?),
                    _ => unreachable!(),
                }
            }
            _ => return Err("Usage: VoicePromptRustPilot [--self-test] [--instance-key key] [--ready-file path] [--run-for-seconds 1-3600] | --quit --instance-key key | --protect-stdin | --unprotect-stdin".into()),
        }
        position += 1;
    }
    if quit {
        if options.ready_file.is_some() || options.run_for_seconds.is_some() || options.self_test {
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
        std::process::exit(2);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("VoicePromptRustPilot requires Windows. The audio core remains platform-neutral.");
    std::process::exit(2);
}
