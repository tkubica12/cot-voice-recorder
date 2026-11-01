use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, OwnedMutexGuard, watch};

use crate::ProtocolError;
use crate::auth::{CachedTokens, DesktopClientConfig, PkcePair, RefreshOutcome, authorization_url};
use crate::loopback::{LoopbackError, LoopbackListener};
use crate::network::{GoogleTokenClient, TokenNetworkError};
use crate::state::{AuthSession, AuthState, TokenDecision, TransitionError};

pub trait TokenPersistence: Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;
    fn load(&self) -> Result<Option<CachedTokens>, Self::Error>;
    fn save(&self, tokens: &CachedTokens) -> Result<(), Self::Error>;
    fn clear(&self) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum AuthRuntimeError<E> {
    NotConfigured,
    Protocol(ProtocolError),
    Network(TokenNetworkError),
    Callback(LoopbackError),
    Storage(E),
    Worker(tokio::task::JoinError),
}

impl<E: fmt::Display> fmt::Display for AuthRuntimeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("Google desktop sign-in is not configured."),
            Self::Protocol(error) => error.fmt(f),
            Self::Network(error) => error.fmt(f),
            Self::Callback(error) => error.fmt(f),
            Self::Storage(error) => write!(f, "Authentication cache operation failed: {error}"),
            Self::Worker(_) => f.write_str("Authentication storage/callback worker failed."),
        }
    }
}

impl<E: Error + 'static> Error for AuthRuntimeError<E> {}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct AuthRuntime<P: TokenPersistence> {
    config: Option<DesktopClientConfig>,
    client: GoogleTokenClient,
    store: Arc<P>,
    session: Arc<Mutex<AuthSession>>,
    status: watch::Sender<AuthState>,
    active_sign_in: Mutex<Option<Arc<AtomicBool>>>,
}

impl<P: TokenPersistence> AuthRuntime<P> {
    pub async fn initialize(
        config: Option<DesktopClientConfig>,
        client: GoogleTokenClient,
        store: P,
    ) -> Result<Self, AuthRuntimeError<P::Error>> {
        let store = Arc::new(store);
        let loader = store.clone();
        let tokens = tokio::task::spawn_blocking(move || loader.load())
            .await
            .map_err(AuthRuntimeError::Worker)?
            .map_err(AuthRuntimeError::Storage)?;
        let session = AuthSession::new(config.is_some(), tokens);
        let (status, _) = watch::channel(session.state());
        Ok(Self {
            config,
            client,
            store,
            session: Arc::new(Mutex::new(session)),
            status,
            active_sign_in: Mutex::new(None),
        })
    }

    pub fn status(&self) -> watch::Receiver<AuthState> {
        self.status.subscribe()
    }

    pub async fn backend_id_token(&self) -> Result<Option<String>, AuthRuntimeError<P::Error>> {
        let session = self.session.clone().lock_owned().await;
        match session
            .token_decision(time::OffsetDateTime::now_utc(), 300)
            .map_err(AuthRuntimeError::Protocol)?
        {
            TokenDecision::Fresh(token) => return Ok(Some(token.to_owned())),
            TokenDecision::NotConfigured
            | TokenDecision::SignedOut
            | TokenDecision::SignInNeeded => return Ok(None),
            TokenDecision::RefreshRequired(_) => {}
        }
        self.refresh_locked(session).await
    }

    pub async fn force_refresh(&self) -> Result<Option<String>, AuthRuntimeError<P::Error>> {
        let session = self.session.clone().lock_owned().await;
        self.refresh_locked(session).await
    }

    async fn refresh_locked(
        &self,
        session: OwnedMutexGuard<AuthSession>,
    ) -> Result<Option<String>, AuthRuntimeError<P::Error>> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let Some(refresh) = session.refresh_token() else {
            return Ok(None);
        };
        let response = self
            .client
            .refresh(config, refresh)
            .await
            .map_err(AuthRuntimeError::Network)?;
        let store = self.store.clone();
        let (result, state, token) = tokio::task::spawn_blocking(move || {
            let mut session = session;
            let result = session.apply_refresh(response, |tokens| store.save(tokens));
            let token = match session.token_decision(time::OffsetDateTime::now_utc(), 0) {
                Ok(TokenDecision::Fresh(token)) => Some(token.to_owned()),
                _ => None,
            };
            (result, session.state(), token)
        })
        .await
        .map_err(AuthRuntimeError::Worker)?;
        self.status.send_replace(state);
        match map_transition(result)? {
            RefreshOutcome::Success => Ok(token),
            RefreshOutcome::RefreshRevoked
            | RefreshOutcome::NoIdToken
            | RefreshOutcome::Transient => Ok(None),
        }
    }

    pub async fn sign_in(
        &self,
        open_browser: impl FnOnce(&url::Url) -> Result<(), ProtocolError>,
    ) -> Result<(), AuthRuntimeError<P::Error>> {
        let config = self
            .config
            .as_ref()
            .ok_or(AuthRuntimeError::NotConfigured)?;
        let session = self.session.clone().lock_owned().await;
        let pair = PkcePair::create().map_err(AuthRuntimeError::Protocol)?;
        let listener = LoopbackListener::bind().map_err(AuthRuntimeError::Callback)?;
        let redirect_uri = listener.redirect_uri().to_owned();
        let uri = authorization_url(config, &redirect_uri, &pair, None)
            .map_err(AuthRuntimeError::Protocol)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.active_sign_in.lock().await = Some(cancelled.clone());
        let _cancel_on_drop = CancelOnDrop(cancelled.clone());
        open_browser(&uri).map_err(AuthRuntimeError::Protocol)?;
        let expected_state = pair.state.clone();
        let code = tokio::task::spawn_blocking(move || {
            listener.wait_for_code(&expected_state, Duration::from_secs(300), &cancelled)
        })
        .await
        .map_err(AuthRuntimeError::Worker)?
        .map_err(AuthRuntimeError::Callback)?;
        let response = self
            .client
            .exchange_code(config, &code, &pair.verifier, &redirect_uri)
            .await
            .map_err(AuthRuntimeError::Network)?;
        let store = self.store.clone();
        let (result, state) = tokio::task::spawn_blocking(move || {
            let mut session = session;
            let result = session.accept_sign_in(response, |tokens| store.save(tokens));
            (result, session.state())
        })
        .await
        .map_err(AuthRuntimeError::Worker)?;
        self.status.send_replace(state);
        map_transition(result)
    }

    pub async fn sign_out(&self) -> Result<(), AuthRuntimeError<P::Error>> {
        self.cancel_sign_in().await;
        let session = self.session.clone().lock_owned().await;
        let store = self.store.clone();
        let (result, state) = tokio::task::spawn_blocking(move || {
            let mut session = session;
            let result = session.sign_out(|| store.clear());
            (result, session.state())
        })
        .await
        .map_err(AuthRuntimeError::Worker)?;
        self.status.send_replace(state);
        map_transition(result)
    }

    pub async fn cancel_sign_in(&self) {
        if let Some(cancelled) = self.active_sign_in.lock().await.as_ref() {
            cancelled.store(true, Ordering::Release);
        }
    }
}

fn map_transition<T, E>(result: Result<T, TransitionError<E>>) -> Result<T, AuthRuntimeError<E>> {
    result.map_err(|error| match error {
        TransitionError::Protocol(error) => AuthRuntimeError::Protocol(error),
        TransitionError::Persistence(error) => AuthRuntimeError::Storage(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Default)]
    struct MemoryStore(Arc<StdMutex<Option<CachedTokens>>>);

    impl TokenPersistence for MemoryStore {
        type Error = std::io::Error;
        fn load(&self) -> Result<Option<CachedTokens>, Self::Error> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, tokens: &CachedTokens) -> Result<(), Self::Error> {
            *self.0.lock().unwrap() = Some(tokens.clone());
            Ok(())
        }
        fn clear(&self) -> Result<(), Self::Error> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn config(uri: &str) -> DesktopClientConfig {
        let mut config = DesktopClientConfig::parse(
            br#"{"installed":{"client_id":"fixture-id","client_secret":"fixture-secret"}}"#,
        )
        .unwrap();
        config.token_uri = url::Url::parse(uri).unwrap();
        config
    }

    #[tokio::test]
    async fn fresh_cached_id_token_requires_no_http_and_signout_is_durable() {
        let claims = format!(
            "{{\"exp\":{}}}",
            time::OffsetDateTime::now_utc().unix_timestamp() + 3600
        );
        let token = format!(
            "e30.{}.",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
        );
        let store = MemoryStore::default();
        store
            .save(&CachedTokens {
                refresh_token: Some("fixture".into()),
                id_token: Some(token.clone()),
                ..Default::default()
            })
            .unwrap();
        let runtime = AuthRuntime::initialize(
            Some(config("http://127.0.0.1:1/token")),
            GoogleTokenClient::new().unwrap(),
            store.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            runtime.backend_id_token().await.unwrap().as_deref(),
            Some(token.as_str())
        );
        runtime.sign_out().await.unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(runtime.backend_id_token().await.unwrap().is_none());
        assert_eq!(*runtime.status().borrow(), AuthState::SignedOut);
    }

    #[tokio::test]
    async fn real_local_signin_runs_callback_exchange_persistence_and_id_token_delivery() {
        let claims = format!(
            "{{\"exp\":{}}}",
            time::OffsetDateTime::now_utc().unix_timestamp() + 3600
        );
        let token = format!(
            "e30.{}.",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
        );
        let body = serde_json::json!({"id_token":token,"refresh_token":"fixture-refresh","access_token":"not-a-backend-token"}).to_string();
        let (endpoint, server) = crate::network::tests::endpoint("200 OK", body.as_bytes());
        let store = MemoryStore::default();
        let runtime = AuthRuntime::initialize(
            Some(config(&endpoint)),
            GoogleTokenClient::for_loopback_tests(),
            store.clone(),
        )
        .await
        .unwrap();
        let callback_thread = StdMutex::new(None);
        runtime.sign_in(|uri| {
            let fields: std::collections::HashMap<_, _> = uri.query_pairs().into_owned().collect();
            let redirect = url::Url::parse(&fields["redirect_uri"]).unwrap();
            let state = fields["state"].clone();
            *callback_thread.lock().unwrap() = Some(std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap())).unwrap();
                stream.write_all(format!("GET /?state={state}&code=fixture-code HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(response.contains("200 OK"));
            }));
            Ok(())
        }).await.unwrap();
        callback_thread
            .into_inner()
            .unwrap()
            .unwrap()
            .join()
            .unwrap();
        let request = server.join().unwrap();
        assert!(request.contains("grant_type=authorization_code"));
        assert!(request.contains("code_verifier="));
        assert_eq!(
            store.load().unwrap().unwrap().refresh_token.as_deref(),
            Some("fixture-refresh")
        );
        assert_eq!(
            runtime.backend_id_token().await.unwrap().as_deref(),
            Some(token.as_str())
        );
        assert_eq!(*runtime.status().borrow(), AuthState::SignedIn);
    }

    #[tokio::test]
    async fn real_local_refresh_revocation_never_returns_an_access_token() {
        let (endpoint, server) = crate::network::tests::endpoint(
            "400 Bad Request",
            br#"{"error":"invalid_grant","access_token":"not-a-backend-token"}"#,
        );
        let store = MemoryStore::default();
        store
            .save(&CachedTokens {
                refresh_token: Some("fixture".into()),
                ..Default::default()
            })
            .unwrap();
        let runtime = AuthRuntime::initialize(
            Some(config(&endpoint)),
            GoogleTokenClient::for_loopback_tests(),
            store.clone(),
        )
        .await
        .unwrap();
        assert!(runtime.backend_id_token().await.unwrap().is_none());
        server.join().unwrap();
        assert_eq!(*runtime.status().borrow(), AuthState::SignInNeeded);
        assert!(store.load().unwrap().unwrap().id_token.is_none());
    }
}
