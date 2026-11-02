use std::process::Command;
fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voice-recorder"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}
#[test]
fn cli_help_and_invalid_config_never_need_identity_or_touch_cloud_state() {
    let help = cli(&["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("check-config")
    );
    let missing = cli(&["check-config"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("validation")
    );
    for args in [
        &["unknown"][..],
        &["worker", "--port", "1"],
        &["api", "--port", "invalid"],
    ] {
        assert!(!cli(args).status.success());
    }
}
#[test]
fn explicit_configuration_validation_is_network_free_and_production_fakes_are_refused() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voice-recorder"));
    command.arg("check-config").env_clear().envs([
        ("VR_GOOGLE_ALLOWED_AUDIENCES", "synthetic-fixture"),
        ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid"),
        ("VR_STORAGE_ACCOUNT_NAME", "fixture123"),
        ("VR_FOUNDRY_ENDPOINT", "https://fixture.invalid"),
        ("VR_SPEECH_ENDPOINT", "https://fixture.invalid"),
        ("VR_WEBPUBSUB_ENDPOINT", "https://fixture.invalid"),
    ]);
    let valid = command.output().unwrap();
    assert!(valid.status.success(), "{:?}", valid.stderr);
    assert!(
        String::from_utf8(valid.stdout)
            .unwrap()
            .contains("no network")
    );
    command.env("VR_USE_FAKE_AI", "true");
    assert!(!command.output().unwrap().status.success());
}
