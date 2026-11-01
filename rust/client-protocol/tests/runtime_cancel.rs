use std::sync::Arc;
use std::time::Duration;

use voiceprompt_client_protocol::auth::{CachedTokens, DesktopClientConfig};
use voiceprompt_client_protocol::loopback::LoopbackError;
use voiceprompt_client_protocol::network::GoogleTokenClient;
use voiceprompt_client_protocol::runtime::{AuthRuntime, AuthRuntimeError, TokenPersistence};
use voiceprompt_client_protocol::state::AuthState;

struct EmptyStore;

impl TokenPersistence for EmptyStore {
    type Error = std::io::Error;
    fn load(&self) -> Result<Option<CachedTokens>, Self::Error> {
        Ok(None)
    }
    fn save(&self, _: &CachedTokens) -> Result<(), Self::Error> {
        Ok(())
    }
    fn clear(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[tokio::test]
async fn cancelling_an_active_signin_stops_its_loopback_wait() {
    let mut config = DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"fixture","client_secret":"fixture"}}"#,
    )
    .unwrap();
    config.token_uri = url::Url::parse("http://127.0.0.1:1/token").unwrap();
    let runtime = Arc::new(
        AuthRuntime::initialize(Some(config), GoogleTokenClient::new().unwrap(), EmptyStore)
            .await
            .unwrap(),
    );
    let (started, receiver) = tokio::sync::oneshot::channel();
    let worker = runtime.clone();
    let attempt = tokio::spawn(async move {
        worker
            .sign_in(|_| {
                let _ = started.send(());
                Ok(())
            })
            .await
    });
    receiver.await.unwrap();
    runtime.cancel_sign_in().await;
    let result = tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(AuthRuntimeError::Callback(LoopbackError::Cancelled))
    ));
    assert_eq!(*runtime.status().borrow(), AuthState::SignedOut);
}
