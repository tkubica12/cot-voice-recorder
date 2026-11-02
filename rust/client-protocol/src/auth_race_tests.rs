use super::*;
use base64::Engine;
use std::io;
use std::sync::{Condvar, Mutex as StdMutex};

struct BlockingStore {
    tokens: StdMutex<Option<CachedTokens>>,
    save_started: tokio::sync::Notify,
    released: StdMutex<bool>,
    wake: Condvar,
}

impl BlockingStore {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

impl TokenPersistence for Arc<BlockingStore> {
    type Error = io::Error;
    fn load(&self) -> Result<Option<CachedTokens>, Self::Error> {
        Ok(self.tokens.lock().unwrap().clone())
    }
    fn save(&self, tokens: &CachedTokens) -> Result<(), Self::Error> {
        *self.tokens.lock().unwrap() = Some(tokens.clone());
        self.save_started.notify_one();
        let released = self.released.lock().unwrap();
        drop(
            self.wake
                .wait_while(released, |released| !*released)
                .unwrap(),
        );
        Ok(())
    }
    fn clear(&self) -> Result<(), Self::Error> {
        *self.tokens.lock().unwrap() = None;
        Ok(())
    }
}

struct ReleaseOnDrop(Arc<BlockingStore>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn token() -> String {
    let claims = format!(
        "{{\"exp\":{}}}",
        time::OffsetDateTime::now_utc().unix_timestamp() + 3600
    );
    format!(
        "e30.{}.",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
    )
}

#[tokio::test]
async fn delayed_refresh_cannot_publish_signed_in_or_vend_old_bearer_after_signout() {
    let response = serde_json::json!({"id_token":token()}).to_string();
    let (endpoint, server) = crate::network::tests::endpoint("200 OK", response.as_bytes());
    let mut config = DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"fixture","client_secret":"fixture"}}"#,
    )
    .unwrap();
    config.token_uri = url::Url::parse(&endpoint).unwrap();
    let store = Arc::new(BlockingStore {
        tokens: StdMutex::new(Some(CachedTokens {
            refresh_token: Some("fixture-refresh".into()),
            id_token: Some(token()),
            email: Some("synthetic@example.invalid".into()),
            ..Default::default()
        })),
        save_started: tokio::sync::Notify::new(),
        released: StdMutex::new(false),
        wake: Condvar::new(),
    });
    let _release = ReleaseOnDrop(store.clone());
    let auth = AuthRuntime::initialize(
        Some(config),
        GoogleTokenClient::for_loopback_tests(),
        store.clone(),
    )
    .await
    .unwrap();
    let mut refresh = Box::pin(auth.force_refresh());
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut refresh => panic!("refresh finished before controlled persistence: {result:?}"),
            _ = store.save_started.notified() => {}
        }
    }).await.unwrap();
    assert_eq!(
        auth.display_identity(),
        (
            AuthState::SignedIn,
            Some("synthetic@example.invalid".into())
        ),
        "Display reads must remain available while session persistence holds the lock."
    );
    store.release();
    tokio::time::timeout(Duration::from_secs(5), auth.sign_out())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*auth.status().borrow(), AuthState::SignedOut);
    assert_eq!(auth.display_identity(), (AuthState::SignedOut, None));
    let bearer = tokio::time::timeout(Duration::from_secs(5), refresh)
        .await
        .unwrap()
        .unwrap();
    assert!(bearer.is_none());
    assert_eq!(*auth.status().borrow(), AuthState::SignedOut);
    assert!(auth.backend_id_token().await.unwrap().is_none());
    assert!(store.tokens.lock().unwrap().is_none());
    server.join().unwrap();
}
