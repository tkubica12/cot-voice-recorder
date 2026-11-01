use super::*;
use crate::http_fixture::{config, serve, token};

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback error response type"
)]
async fn actual_websocket_handshake_initialization_and_pcm_events_use_only_server_owned_credentials()
 {
    let mut identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = MaiStreamingProvider::new(
        &config(&endpoint),
        Arc::new(ManagedIdentity::fixture(identity.endpoint.as_str())),
        "fixture-model",
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.uri().to_string(),
                    "/mai/v1/realtime?intent=transcription"
                );
                assert_eq!(
                    request.headers()["Authorization"],
                    "Bearer fixture-data-plane"
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await
            .unwrap();
        let Message::Text(update) = socket.next().await.unwrap().unwrap() else {
            panic!("Missing session update");
        };
        let update: Value = serde_json::from_str(&update).unwrap();
        assert_eq!(
            update,
            serde_json::json!({"type":"session.update","session":{"type":"transcription",
            "audio":{"input":{"format":{"type":"audio/pcm","rate":16000}, "transcription":{"model":"fixture-model","language":"cs"},
                "turn_detection":null,"noise_reduction":null}}}})
        );
        socket
            .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
            .await
            .unwrap();
        let Message::Text(audio) = socket.next().await.unwrap().unwrap() else {
            panic!("Missing audio");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&audio).unwrap()["audio"],
            "AQIDBA=="
        );
        socket
            .send(Message::Text(
                r#"{"type":"input_audio_buffer.committed","item_id":"synthetic"}"#.into(),
            ))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let mut upstream = provider.session("cs").await.unwrap();
    upstream
        .send(serde_json::json!({"type":"input_audio_buffer.append","audio":"AQIDBA=="}))
        .await
        .unwrap();
    assert_eq!(upstream.recv().await.unwrap()["item_id"], "synthetic");
    upstream.close().await.unwrap();
    let request = identity.next().await;
    assert!(
        request.target.query_pairs().any(
            |(key, value)| key == "resource" && value == "https://cognitiveservices.azure.com/"
        )
    );
    identity.finish().await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn configuration_rejection_and_malformed_events_do_not_expose_provider_data() {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = MaiStreamingProvider::new(
        &config(&endpoint),
        Arc::new(ManagedIdentity::fixture(identity.endpoint.as_str())),
        "fixture-model",
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"error","message":"private provider text"}"#.into(),
            ))
            .await
            .unwrap();
    });
    let error = provider.session("auto").await.err().unwrap();
    assert_eq!(error.status, 503);
    assert!(!error.detail.contains("private"));
    server.await.unwrap();
    identity.finish().await;
}
#[test]
fn provider_rewrites_the_resource_hostname_but_rejects_non_root_configuration() {
    let mut config = crate::http_fixture::config(
        &"https://fixture.cognitiveservices.azure.com/"
            .parse()
            .unwrap(),
    );
    let identity = Arc::new(ManagedIdentity::fixture("http://127.0.0.1:1234/token"));
    let provider = MaiStreamingProvider::new(&config, identity.clone(), "fixture-model").unwrap();
    assert_eq!(
        provider.endpoint.as_str(),
        "wss://fixture.services.ai.azure.com/mai/v1/realtime?intent=transcription"
    );
    config.foundry.set_path("/not-root");
    assert!(MaiStreamingProvider::new(&config, identity, "fixture-model").is_err());
}

#[tokio::test]
async fn missing_session_created_is_bounded_to_fifteen_seconds_and_does_not_leak_a_socket() {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = MaiStreamingProvider::new(
        &config(&endpoint),
        Arc::new(ManagedIdentity::fixture(identity.endpoint.as_str())),
        "fixture-model",
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert!(socket.next().await.is_none_or(|result| result.is_err()));
    });
    let started = tokio::time::Instant::now();
    assert_eq!(provider.session("auto").await.err().unwrap().status, 503);
    assert!(started.elapsed() >= Duration::from_secs(15));
    assert!(started.elapsed() < Duration::from_secs(20));
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
}

#[tokio::test]
async fn malformed_upstream_json_is_terminal_and_auto_language_is_not_sent_as_an_upstream_locale() {
    for data in ["[]", "private malformed provider text"] {
        let identity = serve(vec![token()]).await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let provider = MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(ManagedIdentity::fixture(identity.endpoint.as_str())),
            "fixture-model",
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"type":"session.created"}"#.into()))
                .await
                .unwrap();
            let update = socket.next().await.unwrap().unwrap();
            let update: Value = serde_json::from_str(update.to_text().unwrap()).unwrap();
            assert!(
                update["session"]["audio"]["input"]["transcription"]
                    .get("language")
                    .is_none()
            );
            socket
                .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
                .await
                .unwrap();
            socket.send(Message::Text(data.into())).await.unwrap();
        });
        let mut connection = provider.session("auto").await.unwrap();
        let error = connection.recv().await.err().unwrap();
        assert_eq!(error.status, 503);
        assert!(!error.detail.contains("private"));
        server.await.unwrap();
        identity.finish().await;
    }
}
