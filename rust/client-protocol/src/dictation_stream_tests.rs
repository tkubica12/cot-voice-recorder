use super::*;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

#[allow(clippy::result_large_err)]
fn accept(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    assert!(
        request.headers()["Authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ")
    );
    assert_eq!(request.uri().path(), "/v1/dictation/stream");
    Ok(response)
}

#[allow(clippy::result_large_err)]
fn unauthorized(_: &Request, _: Response) -> Result<Response, ErrorResponse> {
    let mut response = ErrorResponse::new(Some("synthetic unauthorized".into()));
    *response.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
    Err(response)
}

async fn client(port: u16) -> ApiClient<crate::api::tests::Store> {
    crate::api::tests::make_client(url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap())
        .await
        .0
}

#[tokio::test]
async fn real_proxy_handshake_sends_pcm_and_finish_and_keeps_provisional_distinct() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accept)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"ready","protocol":1,"start_byte":6400}"#.into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Binary(vec![1; 640].into())
        );
        let finish = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(finish.to_text().unwrap()).unwrap()["type"],
            "finish"
        );
        for event in [
            r#"{"type":"delta","item_id":"i","text":"synthetic"}"#,
            r#"{"type":"intermediate","item_id":"i","text":"synthetic preview"}"#,
            r#"{"type":"confirmed","item_id":"i","text":"synthetic final","byte_end":7040}"#,
            r#"{"type":"done","byte_end":7040}"#,
        ] {
            socket.send(Message::Text(event.into())).await.unwrap();
        }
    });
    let client = client(port).await;
    let (sender, mut receiver) = client.connect_dictation("cs", 6400).await.unwrap().split();
    sender.audio(&[1; 640]).await.unwrap();
    sender.commit(true).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        DictationMessage::Delta { .. }
    ));
    assert!(matches!(
        receiver.receive().await.unwrap(),
        DictationMessage::Intermediate { .. }
    ));
    assert!(matches!(
        receiver.receive().await.unwrap(),
        DictationMessage::Confirmed { byte_end: 7040, .. }
    ));
    assert!(matches!(
        receiver.receive().await.unwrap(),
        DictationMessage::Done { byte_end: 7040 }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn signout_invalidates_both_sides_of_an_existing_dictation_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accept)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"ready","protocol":1,"start_byte":0}"#.into(),
            ))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let client = client(port).await;
    let (sender, mut receiver) = client.connect_dictation("auto", 0).await.unwrap().split();
    client.auth_for_tests().sign_out().await.unwrap();
    assert_eq!(
        sender.audio(&[0; 640]).await.unwrap_err().kind,
        ApiErrorKind::Unauthorized
    );
    assert_eq!(
        receiver.receive().await.unwrap_err().kind,
        ApiErrorKind::Unauthorized
    );
    drop(sender);
    drop(receiver);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn wrong_ready_cursor_is_rejected_and_invalid_capture_config_never_connects() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accept)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"ready","protocol":1,"start_byte":2}"#.into(),
            ))
            .await
            .unwrap();
    });
    let client = client(port).await;
    assert!(
        client
            .connect_dictation("auto", 0)
            .await
            .err()
            .unwrap()
            .response_invalid
    );
    assert_eq!(
        client
            .connect_dictation("unsupported", 0)
            .await
            .err()
            .unwrap()
            .kind,
        ApiErrorKind::BadRequest
    );
    assert_eq!(
        client
            .connect_dictation("auto", 1)
            .await
            .err()
            .unwrap()
            .kind,
        ApiErrorKind::BadRequest
    );
    server.await.unwrap();
}

#[test]
fn errors_odd_cursors_bad_shapes_and_ack_never_become_confirmed_text() {
    for data in [
        br#"{"type":"ack","byte_end":640}"#.as_slice(),
        br#"{"type":"confirmed","item_id":"i","text":"x","byte_end":1}"#,
        br#"{"type":"delta","item_id":"i","text":null}"#,
        br#"{"type":"ready","protocol":1,"start_byte":1}"#,
        b"corrupt",
    ] {
        assert!(DictationMessage::parse(data).unwrap_err().response_invalid);
    }
}

#[tokio::test]
async fn unauthorized_handshake_refreshes_once_before_a_new_ready_connection() {
    use base64::Engine;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let claims = serde_json::json!({
        "exp":time::OffsetDateTime::now_utc().unix_timestamp()+3600,
        "sub":"synthetic-updated",
    })
    .to_string();
    let updated = format!(
        "e30.{}.",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
    );
    let response = serde_json::json!({"id_token":updated}).to_string();
    let (token_endpoint, token_server) =
        crate::network::tests::endpoint("200 OK", response.as_bytes());
    let expected = updated.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        assert!(
            tokio_tungstenite::accept_hdr_async(stream, unauthorized)
                .await
                .is_err()
        );
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            #[allow(clippy::result_large_err)]
            |request: &Request, response: Response| {
                assert_eq!(
                    request.headers()["Authorization"],
                    format!("Bearer {expected}")
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"ready","protocol":1,"start_byte":0}"#.into(),
            ))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let (client, _) = crate::api::tests::make_client_with_token_endpoint(
        url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
        Some(url::Url::parse(&token_endpoint).unwrap()),
    )
    .await;
    let connection = client.connect_dictation("auto", 0).await.unwrap();
    drop(connection);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    token_server.join().unwrap();
}
#[test]
fn streaming_errors_are_classified_and_text_debug_is_redacted() {
    for (code, expected) in [
        ("unauthorized", ApiErrorKind::Unauthorized),
        ("forbidden", ApiErrorKind::Forbidden),
        ("too_many_requests", ApiErrorKind::Retryable),
    ] {
        let message = serde_json::json!({"type":"error","code":code}).to_string();
        assert_eq!(
            DictationMessage::parse(message.as_bytes())
                .unwrap_err()
                .kind,
            expected
        );
    }
    let event =
        DictationMessage::parse(br#"{"type":"delta","item_id":"i","text":"synthetic private"}"#)
            .unwrap();
    assert!(!format!("{event:?}").contains("synthetic"));
}
