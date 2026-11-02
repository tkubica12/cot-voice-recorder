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

#[cfg(feature = "loopback-fixtures")]
#[path = "fixtures/azure.rs"]
mod azure;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let jwks = std::env::args()
        .nth(1)
        .ok_or("Pass the local synthetic JWKS URL.")?;
    let uri: url::Url = jwks.parse()?;
    if uri.scheme() != "http" || uri.host_str() != Some("127.0.0.1") {
        return Err("Only an explicit loopback JWKS fixture is allowed.".into());
    }
    let extra: Vec<_> = std::env::args().skip(2).collect();
    #[cfg(feature = "loopback-fixtures")]
    let fixture = azure::Fixture::arguments(&extra)?;
    #[cfg(not(feature = "loopback-fixtures"))]
    if !extra.is_empty() {
        return Err(
            "Storage overrides require the explicit loopback-fixtures example build.".into(),
        );
    }
    let values = [
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
    .collect();
    #[cfg(feature = "loopback-fixtures")]
    let values = {
        let mut values = values;
        if let Some(fixture) = &fixture {
            fixture.configure(&mut values);
        }
        values
    };
    let config = Arc::new(Config::parse(&values)?);
    #[cfg(feature = "loopback-fixtures")]
    let storage = fixture.as_ref().map(azure::Fixture::storage).transpose()?;
    #[cfg(not(feature = "loopback-fixtures"))]
    let storage: Option<Arc<voiceprompt_cloud::storage::AzureStorage>> = None;
    let azure_backend = storage.is_some();
    let (repository, blobs, queue) = if let Some(storage) = storage {
        storage.check_resources(&config).await?;
        (
            Repository::Azure(storage.clone()),
            Blobs::Azure(storage.clone()),
            Queue::Azure {
                storage,
                name: config.queue.clone(),
            },
        )
    } else {
        (Repository::memory(), Blobs::memory(), Queue::memory())
    };
    let services = Arc::new(Services::new(config.clone(), repository, blobs, queue)?);
    let state = ApiState::new(services, Arc::new(GoogleVerifier::new(config)?));
    state.ready.store(true, Ordering::Release);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    println!(
        "{}",
        serde_json::json!({
            "url":format!("http://{}",listener.local_addr()?),
            "storage_backend":if azure_backend { "azure" } else { "memory" },
        })
    );
    std::io::stdout().flush()?;
    tokio::time::timeout(
        std::time::Duration::from_secs(600),
        axum::serve(listener, router(state)).into_future(),
    )
    .await??;
    Ok(())
}
