fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("assets")
        .join("windows")
        .join("app.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    winresource::WindowsResource::new()
        .set_icon(
            icon.to_str()
                .expect("Original VoicePrompt icon path must be UTF-8."),
        )
        .set("ProductName", "VoicePrompt")
        .set("FileDescription", "VoicePrompt native Windows client")
        .set("OriginalFilename", "VoicePrompt.exe")
        .compile()
        .expect(
            "Embed the original VoicePrompt icon using the existing Windows SDK resource compiler.",
        );
}
