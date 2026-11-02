use std::io::Write;
use std::sync::{Arc, atomic::Ordering};
use voiceprompt_cloud::{
    api::{ApiState, router},
    auth::GoogleVerifier,
    config::Config,
    ports::{Blobs, Queue},
    repository::Repository,
    services::Services,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let jwks = std::env::args()
        .nth(1)
        .ok_or("Pass the local synthetic JWKS URL.")?;
    let uri: url::Url = jwks.parse()?;
    if uri.scheme() != "http" || uri.host_str() != Some("127.0.0.1") {
        return Err("Only an explicit loopback JWKS fixture is allowed.".into());
    }
    let config = Arc::new(Config::parse(
        &[
            ("VR_ENVIRONMENT", "test".into()),
            ("VR_GOOGLE_ALLOWED_AUDIENCES", "fixture".into()),
            ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid".into()),
            ("VR_STORAGE_ACCOUNT_NAME", "fixture123".into()),
            ("VR_GOOGLE_JWKS_URI", jwks),
            ("VR_FOUNDRY_ENDPOINT", "https://fixture.invalid".into()),
            ("VR_SPEECH_ENDPOINT", "https://fixture.invalid".into()),
            ("VR_WEBPUBSUB_ENDPOINT", "https://fixture.invalid".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect(),
    )?);
    let services = Arc::new(Services::new(
        config.clone(),
        Repository::memory(),
        Blobs::memory(),
        Queue::memory(),
    )?);
    let state = ApiState::new(services, Arc::new(GoogleVerifier::new(config)?));
    state.ready.store(true, Ordering::Release);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    println!(
        "{}",
        serde_json::json!({"url":format!("http://{}",listener.local_addr()?)})
    );
    std::io::stdout().flush()?;
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        axum::serve(listener, router(state)).into_future(),
    )
    .await??;
    Ok(())
}
