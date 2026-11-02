use std::ffi::OsString;
use std::path::PathBuf;
use voiceprompt_windows_pilot::{ClientOptions, TrayOptions};

pub enum Startup {
    Installed { quit: bool },
    Isolated { options: TrayOptions, quit: bool },
}

pub fn installed_options(state_directory: PathBuf, oauth_config: Option<PathBuf>) -> TrayOptions {
    TrayOptions {
        instance_key: "interactive".into(),
        ready_file: None,
        fixture_ready_file: None,
        run_for_seconds: None,
        self_test: false,
        client: Some(ClientOptions {
            state_directory,
            oauth_config,
            clipboard_enabled: true,
            installed: true,
            fixture_runtime: false,
        }),
    }
}

pub fn parse(
    args: &[OsString],
    packaged_executable: bool,
) -> Result<Startup, Box<dyn std::error::Error>> {
    let mut options = TrayOptions {
        instance_key: "interactive".into(),
        ready_file: None,
        fixture_ready_file: None,
        run_for_seconds: None,
        self_test: false,
        client: None,
    };
    let mut state_directory = None;
    let mut oauth_config = None;
    let mut installed = packaged_executable;
    let mut explicit_instance = false;
    let mut fixture_runtime = false;
    let mut quit = false;
    let mut position = 0;
    while position < args.len() {
        match args[position].to_str() {
            Some("--self-test") => options.self_test = true,
            Some("--quit") => quit = true,
            Some("--installed") => installed = true,
            Some("--instance-key" | "--ready-file" | "--fixture-ready-file" | "--signed-out-fixture" | "--run-for-seconds" | "--state-directory" | "--oauth-config") => {
                let option = args[position].to_str().ok_or("Invalid option encoding.")?;
                position += 1;
                let value = args.get(position).ok_or("Missing option value.")?;
                match option {
                    "--instance-key" => {
                        explicit_instance = true;
                        options.instance_key = value.to_str().ok_or("Instance key must be ASCII.")?.into();
                    }
                    "--ready-file" => options.ready_file = Some(PathBuf::from(value)),
                    "--fixture-ready-file"=>options.fixture_ready_file=Some(PathBuf::from(value)),
                    "--signed-out-fixture"=>{
                        if state_directory.is_some(){return Err("Measurement needs one unambiguous explicit state directory.".into());}
                        state_directory=Some(PathBuf::from(value));fixture_runtime=true;
                    }
                    "--run-for-seconds" => options.run_for_seconds = Some(value.to_str().ok_or("Invalid duration encoding.")?.parse()?),
                    "--state-directory" => {
                        if fixture_runtime{return Err("Measurement cannot override its marked state directory.".into());}
                        state_directory = Some(PathBuf::from(value));
                    }
                    "--oauth-config" => oauth_config = Some(PathBuf::from(value)),
                    _ => unreachable!(),
                }
            }
            _ => return Err("Usage: VoicePromptRustPilot [--self-test] [--instance-key key] [--ready-file path] [--run-for-seconds 1-3600] [--state-directory absolute-path [--oauth-config path]] | --installed [--quit] | --quit --instance-key key | explicit-path fixture commands".into()),
        }
        position += 1;
    }
    if installed {
        if explicit_instance
            || options.self_test
            || options.ready_file.is_some()
            || options.run_for_seconds.is_some()
            || options.fixture_ready_file.is_some()
            || state_directory.is_some()
            || oauth_config.is_some()
        {
            return Err(
                "Installed mode cannot be combined with fixture/benchmark/explicit-state startup."
                    .into(),
            );
        }
        return Ok(Startup::Installed { quit });
    }
    if oauth_config.is_some() && state_directory.is_none() {
        return Err("--oauth-config requires an explicit --state-directory.".into());
    }
    if fixture_runtime
        && (!explicit_instance
            || options.fixture_ready_file.is_none()
            || options.run_for_seconds.is_none()
            || options.self_test
            || oauth_config.is_some()
            || quit
            || options.ready_file.is_some())
    {
        return Err("Signed-out full-client measurement needs a key, separate readiness path and bounded duration, with no OAuth/self-test/quit.".into());
    }
    if options.fixture_ready_file.is_some() && !fixture_runtime {
        return Err("Client measurement readiness requires --signed-out-fixture.".into());
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
            clipboard_enabled: !options.self_test && !fixture_runtime,
            installed: false,
            fixture_runtime,
        });
    }
    if quit
        && (options.ready_file.is_some()
            || options.run_for_seconds.is_some()
            || options.self_test
            || options.client.is_some())
    {
        return Err("--quit cannot be combined with startup options.".into());
    }
    Ok(Startup::Isolated { options, quit })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn installed_selection_does_not_depend_on_having_a_value_option() {
        assert!(matches!(
            parse(&arguments(&["--installed"]), false).unwrap(),
            Startup::Installed { quit: false }
        ));
        assert!(matches!(
            parse(&[], true).unwrap(),
            Startup::Installed { quit: false }
        ));
        assert!(matches!(
            parse(&arguments(&["--quit"]), true).unwrap(),
            Startup::Installed { quit: true }
        ));
        for values in [&["--installed", "--quit"][..], &["--quit", "--installed"]] {
            assert!(matches!(
                parse(&arguments(values), false).unwrap(),
                Startup::Installed { quit: true }
            ));
        }
    }
    #[test]
    fn installed_plan_always_connects_the_personal_state_and_compatible_instance_mode() {
        let path = PathBuf::from(r"C:\synthetic-fixture\VoicePrompt");
        let oauth = path.join("google-desktop-client.json");
        let plan = installed_options(path.clone(), Some(oauth.clone()));
        assert!(plan.ready_file.is_none() && !plan.self_test);
        let client = plan.client.unwrap();
        assert!(client.installed && client.clipboard_enabled);
        assert_eq!(client.state_directory, path);
        assert_eq!(client.oauth_config, Some(oauth));
    }
    #[test]
    fn installed_fixture_conflicts_are_order_independent_before_any_io() {
        for flags in [
            &["--self-test"][..],
            &["--instance-key", "fixture"],
            &["--run-for-seconds", "1"],
            &["--ready-file", "fixture"],
            &["--state-directory", r"C:\fixture"],
            &["--oauth-config", "fixture"],
            &["--fixture-ready-file", "fixture"],
            &["--signed-out-fixture", r"C:\fixture"],
        ] {
            let mut values = arguments(flags);
            values.push("--installed".into());
            assert!(parse(&values, false).is_err());
            values.pop();
            values.insert(0, "--installed".into());
            assert!(parse(&values, false).is_err());
            assert!(parse(&arguments(flags), true).is_err());
        }
    }

    #[test]
    fn full_client_measurement_needs_its_own_scope_and_blocks_all_physical_ports() {
        let args = arguments(&[
            "--signed-out-fixture",
            r"C:\fixture",
            "--fixture-ready-file",
            r"C:\ready",
            "--instance-key",
            "0123456789abcdef0123456789abcdef",
            "--run-for-seconds",
            "2",
        ]);
        let Startup::Isolated { options, quit } = parse(&args, false).unwrap() else {
            panic!("Expected isolated full fixture");
        };
        assert!(!quit && !options.self_test && options.ready_file.is_none());
        assert_eq!(options.fixture_ready_file, Some(PathBuf::from(r"C:\ready")));
        let client = options.client.unwrap();
        assert!(
            client.fixture_runtime
                && !client.clipboard_enabled
                && !client.installed
                && client.oauth_config.is_none()
        );
        for values in [
            &["--fixture-ready-file", r"C:\ready"][..],
            &["--signed-out-fixture", r"C:\fixture"],
            &["--signed-out-fixture", r"C:\fixture", "--self-test"],
            &[
                "--signed-out-fixture",
                r"C:\fixture",
                "--oauth-config",
                "fixture",
            ],
        ] {
            assert!(parse(&arguments(values), false).is_err());
        }
    }
    #[test]
    fn isolated_client_and_quit_remain_separate_from_installed_state() {
        let Startup::Isolated { options, quit } = parse(
            &arguments(&[
                "--self-test",
                "--instance-key",
                "fixture",
                "--state-directory",
                r"C:\fixture",
            ]),
            false,
        )
        .unwrap() else {
            panic!("Expected isolated startup");
        };
        assert!(!quit);
        let client = options.client.unwrap();
        assert!(!client.installed && !client.clipboard_enabled);
        assert_eq!(client.state_directory, PathBuf::from(r"C:\fixture"));
        assert!(matches!(
            parse(&arguments(&["--quit", "--instance-key", "fixture"]), false).unwrap(),
            Startup::Isolated { quit: true, .. }
        ));
    }
    #[test]
    fn ambiguous_or_incomplete_arguments_never_become_a_successful_plan() {
        for values in [
            &["--oauth-config", "fixture"][..],
            &["--state-directory"],
            &["--unknown"],
            &[
                "--ready-file",
                "fixture",
                "--state-directory",
                r"C:\fixture",
            ],
            &["--self-test", "--quit"],
        ] {
            assert!(parse(&arguments(values), false).is_err());
        }
    }
}
