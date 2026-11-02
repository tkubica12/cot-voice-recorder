use super::*;
use futures_util::SinkExt;
use std::sync::Mutex;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use voiceprompt_client_protocol::api::ApiClient;
use voiceprompt_client_protocol::auth::{CachedTokens, DesktopClientConfig};
use voiceprompt_client_protocol::network::GoogleTokenClient;
use voiceprompt_client_protocol::runtime::{AuthRuntime, TokenPersistence};
use voiceprompt_windows_pilot::{
    BoundBatchBackend, DEFAULT_RECOVERY_QUOTA, LegacySession, NativeRecoveryStore, StreamingSession,
};

struct Tokens(Mutex<Option<CachedTokens>>);
impl TokenPersistence for Tokens {
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
#[tokio::test]
async fn native_legacy_chunker_and_durable_session_use_the_actual_rust_http_speech_adapter() {
    let mut speech = crate::http_fixture::serve(vec![
        crate::http_fixture::token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"synthetic legacy final"}]}"#.into(),
        ),
    ])
    .await;
    let bearer = token();
    let mut state = fixture();
    let Verifier::Static {
        token: expected, ..
    } = &mut state.verifier
    else {
        panic!("Synthetic verifier required");
    };
    *expected = bearer.clone();
    state.ai = Some(Arc::new(
        crate::ai::AzureAi::new(
            Arc::new(crate::http_fixture::config(&speech.endpoint)),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                speech.endpoint.as_str(),
            )),
        )
        .unwrap(),
    ));
    let (endpoint, server) = serve_ws(state).await;
    let (api, _auth, label) = client(&endpoint, bearer).await;
    let directory = Root::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "auto".into(),
            format!("{label}\nfixture@example.invalid"),
            false,
            crate::services::now(),
        )
        .unwrap();
    let session = LegacySession::start(
        journal,
        Arc::new(BoundBatchBackend::new(api, label).unwrap()),
    )
    .unwrap();
    session.append(&vec![1; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(8), false)
            .await
            .unwrap()
            .as_deref(),
        Some("synthetic legacy final")
    );
    assert!(session.snapshot().unwrap().capture_complete);
    session.shutdown().await.unwrap();
    speech.next().await;
    let request = speech.next().await;
    assert_eq!(
        voiceprompt_core::wav::capture_pcm(crate::http_fixture::part(&request.body, "audio"))
            .unwrap(),
        vec![1; 6400]
    );
    speech.finish().await;
    server.abort();
}

struct Root(std::path::PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "VoicePrompt-native-cloud-e2e-{}",
            crate::services::uuid().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if std::thread::panicking() {
                eprintln!("Isolated native fixture cleanup failed: {error}");
            } else {
                panic!("Isolated native fixture cleanup failed: {error}");
            }
        }
    }
}
fn token() -> String {
    format!("e30.{}.",base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({"exp":crate::services::now().unix_timestamp()+3600,"email":"fixture@example.invalid"}).to_string()))
}
async fn client(
    endpoint: &str,
    token: String,
) -> (Arc<ApiClient<Tokens>>, Arc<AuthRuntime<Tokens>>, String) {
    let base: url::Url = endpoint.replacen("ws://", "http://", 1).parse().unwrap();
    let label = base.as_str().trim_end_matches('/').to_owned();
    let config = DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"synthetic-fixture","client_secret":"synthetic-fixture"}}"#,
    )
    .unwrap();
    let auth = Arc::new(
        AuthRuntime::initialize(
            Some(config),
            GoogleTokenClient::new().unwrap(),
            Tokens(Mutex::new(Some(CachedTokens {
                refresh_token: Some("synthetic-fixture".into()),
                id_token: Some(token),
                ..Default::default()
            }))),
        )
        .await
        .unwrap(),
    );
    assert!(
        ApiClient::new(base.clone(), auth.clone()).is_err(),
        "The normal constructor must still require HTTPS."
    );
    let api = Arc::new(ApiClient::for_loopback_fixture(base, auth.clone()).unwrap());
    (api, auth, label)
}
async fn server_state(token: String, endpoint: &url::Url, identity: &url::Url) -> ApiState {
    let mut state = fixture();
    let Verifier::Static {
        token: expected, ..
    } = &mut state.verifier
    else {
        panic!("Synthetic verifier required");
    };
    *expected = token;
    state.streaming = Some(Arc::new(
        crate::streaming::MaiStreamingProvider::new(
            &crate::http_fixture::config(endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(identity.as_str())),
            "synthetic-model",
        )
        .unwrap(),
    ));
    state
}

#[tokio::test]
async fn native_durable_session_real_client_relay_and_archive_preserve_exact_audio_confirmed_text_and_fixed_audit()
 {
    let identity = crate::http_fixture::serve(vec![crate::http_fixture::token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let resource: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let bearer = token();
    let state = server_state(bearer.clone(), &resource, &identity.endpoint).await;
    let services = state.services.clone();
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
            .await
            .unwrap();
        let mut pcm = Vec::new();
        while let Some(Ok(Message::Text(data))) = socket.next().await {
            let event: serde_json::Value = serde_json::from_str(&data).unwrap();
            if event["type"] == "input_audio_buffer.append" {
                pcm.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(event["audio"].as_str().unwrap())
                        .unwrap(),
                );
                socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"synthetic","delta":"provisional must not paste"}"#.into())).await.unwrap();
            } else if event["type"] == "input_audio_buffer.commit" {
                socket
                    .send(Message::Text(
                        r#"{"type":"input_audio_buffer.committed","item_id":"synthetic"}"#.into(),
                    ))
                    .await
                    .unwrap();
                socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"synthetic","transcript":"synthetic confirmed text"}"#.into())).await.unwrap();
            }
        }
        assert_eq!(pcm, vec![1; 6400]);
    });
    let (endpoint, server) = serve_ws(state).await;
    let (api, _auth, label) = client(&endpoint, bearer).await;
    let directory = Root::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let context = format!("{label}\nfixture@example.invalid");
    let journal = store
        .create("auto".into(), context, true, crate::services::now())
        .unwrap();
    let id = journal.id().to_owned();
    let session = StreamingSession::start(
        journal,
        Arc::new(BoundBatchBackend::new(api.clone(), label).unwrap()),
        false,
    )
    .unwrap();
    session.append(&vec![1; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(8), false)
            .await
            .unwrap()
            .as_deref(),
        Some("synthetic confirmed text")
    );
    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.confirmed_bytes, 6400);
    assert_eq!(snapshot.confirmed_text, "synthetic confirmed text");
    let completed = crate::services::now();
    session
        .prepare_audit("synthetic final text".into(), completed, true)
        .unwrap();
    let frozen = session.snapshot().unwrap();
    let receipt = api
        .archive_bound_dictation(&frozen, "fixture@example.invalid")
        .await
        .unwrap();
    assert_eq!(receipt.source, "windows_dictation");
    assert_eq!(
        receipt.transcript_id,
        crate::services::canonical_uuid(&id).unwrap()
    );
    let full = services.transcript(&receipt.transcript_id).await.unwrap();
    assert_eq!(full.body, "synthetic final text");
    assert_eq!(full.raw_body.as_deref(), Some("synthetic confirmed text"));
    assert_eq!(full.expires_at, completed + time::Duration::hours(48));
    let crate::ports::Queue::Memory(queue) = &services.queue else {
        panic!("Synthetic queue required");
    };
    assert!(queue.messages.lock().await.is_empty());
    session.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
    server.abort();
}

#[tokio::test]
async fn native_signout_cancels_the_real_relay_without_confirming_provisional_audio_or_losing_recovery()
 {
    let identity = crate::http_fixture::serve(vec![crate::http_fixture::token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let resource: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let bearer = token();
    let state = server_state(bearer.clone(), &resource, &identity.endpoint).await;
    let (seen, received) = tokio::sync::oneshot::channel();
    let slots = state.stream_slots.clone();
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
            .await
            .unwrap();
        let mut seen = Some(seen);
        while let Some(Ok(Message::Text(data))) = socket.next().await {
            let event: serde_json::Value = serde_json::from_str(&data).unwrap();
            if event["type"] == "input_audio_buffer.append" {
                if let Some(seen) = seen.take() {
                    socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"synthetic","delta":"unconfirmed"}"#.into())).await.unwrap();
                    seen.send(()).unwrap();
                }
            }
        }
    });
    let (endpoint, server) = serve_ws(state).await;
    let (api, auth, label) = client(&endpoint, bearer).await;
    let directory = Root::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "auto".into(),
            format!("{label}\nfixture@example.invalid"),
            true,
            crate::services::now(),
        )
        .unwrap();
    let id = journal.id().to_owned();
    let session = StreamingSession::start(
        journal,
        Arc::new(BoundBatchBackend::new(api, label).unwrap()),
        false,
    )
    .unwrap();
    session.append(&vec![2; 6400]).unwrap();
    session.finish_capture().unwrap();
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    auth.sign_out().await.unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_millis(150), false)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(session.snapshot().unwrap().confirmed_bytes, 0);
    session.shutdown().await.unwrap();
    let retained = store.open(&id).unwrap();
    assert!(retained.snapshot().unwrap().captured_bytes >= 6400);
    assert_eq!(
        voiceprompt_core::wav::capture_pcm(&retained.read_audio(0).unwrap()).unwrap(),
        vec![2; 6400]
    );
    tokio::time::timeout(Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
    server.abort();
    tokio::time::timeout(Duration::from_secs(5), async {
        while slots.available_permits() != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
