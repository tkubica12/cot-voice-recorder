use super::*;
use axum::body::Body;
use axum::body::to_bytes;
use base64::Engine;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn fixture() -> ApiState {
    ApiState {
        services: Arc::new(crate::services::tests::fixture()),
        verifier: Verifier::Static {
            token: "synthetic-token".into(),
            user: User {
                email: "fixture@example.invalid".into(),
                subject: "synthetic".into(),
                audience: "fixture".into(),
            },
        },
        ready: Arc::new(AtomicBool::new(false)),
        realtime: None,
        ai: None,
        dictation_pool: Arc::new(crate::ai::ExecutionPool::new(
            4,
            std::time::Duration::from_secs(20),
        )),
        refinement_pool: Arc::new(crate::ai::ExecutionPool::new(
            2,
            std::time::Duration::from_secs(20),
        )),
        streaming: None,
        stream_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        shutdown: tokio::sync::watch::channel(false).0,
    }
}
async fn json_body(response: Response) -> serde_json::Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}
fn request(method: &str, path: &str, data: Body) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer synthetic-token")
        .header("Content-Type", "application/json")
        .body(data)
        .unwrap()
}

#[tokio::test]
async fn readiness_is_not_success_until_bootstrap_and_unauthorized_errors_are_rfc9457() {
    let state = fixture();
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(request("GET", "/health/live", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = app
        .clone()
        .oneshot(request("GET", "/health/ready", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(json_body(response).await["status"], "not_ready");
    state.ready.store(true, Ordering::Release);
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/health/ready", Body::empty()))
            .await
            .unwrap()
            .status(),
        200
    );
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/recordings")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"client_recording_id":"fixture"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.headers()["Content-Type"],
        "application/problem+json"
    );
    assert!(response.headers().contains_key("WWW-Authenticate"));
    assert!(response.headers().contains_key("X-Request-Id"));
    let problem = json_body(response).await;
    assert_eq!(problem["code"], "unauthorized");
    assert_eq!(problem["instance"], "/v1/recordings");
    assert_eq!(problem["status"], 401);
}

#[tokio::test]
async fn actual_rust_router_preserves_recording_upload_completion_and_idempotent_response_shapes() {
    let app = router(fixture());
    let payload = r#"{"client_recording_id":"fixture","transcription_mode":"streaming","audio_layout":"contiguous","refinement_enabled":false}"#;
    let response = app
        .clone()
        .oneshot(request("POST", "/v1/recordings", Body::from(payload)))
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert!(response.headers().contains_key("Location"));
    let record = json_body(response).await;
    let id = record["recording_id"].as_str().unwrap();
    assert_eq!(record["progress"]["received_chunk_count"], 0);
    let response = app
        .clone()
        .oneshot(request("POST", "/v1/recordings", Body::from(payload)))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["recording_id"], id);
    let wav = voiceprompt_core::wav::to_wav(&vec![0; 320000]).unwrap();
    let digest = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    let upload = axum::http::Request::builder()
        .method("PUT")
        .uri(format!("/v1/recordings/{id}/chunks/0"))
        .header("Authorization", "Bearer synthetic-token")
        .header("Content-Type", "audio/wav")
        .header("Content-Digest", digest)
        .header("X-Audio-Start-Sample", "0")
        .header("X-Audio-Sample-Count", "160000")
        .body(Body::from(wav))
        .unwrap();
    assert_eq!(app.clone().oneshot(upload).await.unwrap().status(), 202);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/recordings/{id}/complete"),
            Body::from(r#"{"chunk_count":1}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    assert_eq!(json_body(response).await["state"], "transcribing");
    let response = app
        .oneshot(request(
            "POST",
            &format!("/v1/recordings/{id}/complete"),
            Body::from(r#"{"chunk_count":2}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(json_body(response).await["code"], "complete-conflict");
}

#[tokio::test]
async fn audit_endpoint_returns_real_summary_without_character_count_and_fetches_both_texts() {
    let app = router(fixture());
    let completed = crate::services::python_datetime(crate::services::now(), true);
    let payload = serde_json::json!({
        "text":"synthetic final","raw_text":"synthetic original","completed_at":completed,
        "language":"auto","polished":false,"transcribe_model":"MAI-Transcribe-2-Streaming",
    })
    .to_string();
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/v1/dictation/transcripts/0123456789abcdef0123456789abcdef",
            Body::from(payload),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Cache-Control"], "no-store");
    let summary = json_body(response).await;
    assert!(summary.get("character_count").is_none());
    assert_eq!(summary["source"], "windows_dictation");
    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/v1/transcripts/{}",
                summary["transcript_id"].as_str().unwrap()
            ),
            Body::empty(),
        ))
        .await
        .unwrap();
    let transcript = json_body(response).await;
    assert_eq!(transcript["body"], "synthetic final");
    assert_eq!(transcript["raw_body"], "synthetic original");
}

#[tokio::test]
async fn unknown_routes_methods_and_invalid_payloads_use_problem_not_framework_error_shapes() {
    let app = router(fixture());
    let response = app
        .clone()
        .oneshot(request("GET", "/missing", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(
        json_body(response).await["type"],
        "https://voice-recorder/errors/not-found"
    );
    let response = app
        .clone()
        .oneshot(request("DELETE", "/v1/recordings", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(json_body(response).await["code"], "method-not-allowed");
    let response = app
        .oneshot(request(
            "POST",
            "/v1/recordings",
            Body::from(r#"{"client_recording_id":"fixture","unknown":true}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert_eq!(json_body(response).await["code"], "validation");
}

#[tokio::test]
async fn optional_negotiation_and_ephemeral_validation_never_call_an_unconfigured_model() {
    let app = router(fixture());
    for payload in ["", "null", "{}"] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/realtime/negotiate",
                Body::from(payload),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), 503, "{payload}");
    }
    for path in ["/v1/dictation/refine", "/v1/dictation/transcribe"] {
        let mut input = request("POST", path, Body::from("{}"));
        input
            .headers_mut()
            .insert("Content-Encoding", HeaderValue::from_static("gzip"));
        let response = app.clone().oneshot(input).await.unwrap();
        assert_eq!(response.status(), 415);
        assert_eq!(response.headers()["Cache-Control"], "no-store");
    }
    for payload in [
        r#"{"text":""}"#,
        r#"{"text":" "}"#,
        r#"{"text":"a","text":"b"}"#,
        r#"{"text":"\ud800"}"#,
        r#"{"text":3}"#,
        r#"{"text":"hello","unknown":true}"#,
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", "/v1/dictation/refine", Body::from(payload)))
            .await
            .unwrap();
        assert_eq!(response.status(), 422, "{payload}");
    }
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/dictation/refine",
            Body::from(serde_json::json!({"text":"a".repeat(4001)}).to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    let mut input = request(
        "POST",
        "/v1/dictation/transcribe?language=cs&language=invalid",
        Body::empty(),
    );
    input
        .headers_mut()
        .insert("Content-Type", HeaderValue::from_static("audio/wav"));
    assert_eq!(app.oneshot(input).await.unwrap().status(), 422);
}

#[tokio::test]
async fn body_limits_are_exact_and_interrupted_io_is_not_reported_as_oversized_input() {
    let app = router(fixture());
    let mut input = request("POST", "/v1/dictation/refine", Body::empty());
    input
        .headers_mut()
        .insert("Content-Length", HeaderValue::from_static("32769"));
    assert_eq!(app.clone().oneshot(input).await.unwrap().status(), 413);
    let input = request(
        "POST",
        "/v1/dictation/refine",
        Body::from(" ".repeat(32769)),
    );
    assert_eq!(app.clone().oneshot(input).await.unwrap().status(), 413);
    let input = request(
        "POST",
        "/v1/dictation/refine",
        Body::from(" ".repeat(32768)),
    );
    assert_eq!(app.clone().oneshot(input).await.unwrap().status(), 422);
    let input = request(
        "POST",
        "/v1/dictation/refine",
        Body::from_stream(futures_util::stream::once(async {
            Err::<String, std::io::Error>(std::io::Error::other("synthetic interrupted body"))
        })),
    );
    let response = app.oneshot(input).await.unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(json_body(response).await["code"], "bad-request");
}

#[test]
fn typed_body_validation_keeps_missing_unknown_nested_field_paths_and_rejects_trailing_json() {
    for (payload, field) in [
        (r#"{}"#, "client_recording_id"),
        (r#"{"client_recording_id":"fixture","extra":true}"#, "extra"),
        (
            r#"{"client_recording_id":"fixture","client":{}}"#,
            "client.platform",
        ),
        (
            r#"{"client_recording_id":"fixture","client":{"platform":"windows","extra":true}}"#,
            "client.extra",
        ),
    ] {
        let error = fields_json::<models::CreateRecording>(payload.as_bytes())
            .err()
            .unwrap();
        assert_eq!(error.status, 422);
        assert_eq!(error.errors[0].field, field);
    }
    assert!(
        fields_json::<models::CreateRecording>(br#"{"client_recording_id":"fixture"} {}"#).is_err()
    );
}

#[tokio::test]
async fn complete_ephemeral_http_paths_use_actual_rust_provider_and_return_only_confirmed_safe_text()
 {
    use crate::http_fixture::{config, part, serve, token};
    let mut server = serve(vec![token(), (200, r#"{"combinedPhrases":[{"text":"synthetic dictation"}]}"#.into()),
        (200, serde_json::json!({"choices":[{"finish_reason":"stop", "message":{"content":r#"{"edits":[["current"," is is"," is"]]}"#,"refusal":null}}]}).to_string())]).await;
    let mut state = fixture();
    state.ai = Some(Arc::new(
        crate::ai::AzureAi::new(
            Arc::new(config(&server.endpoint)),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                server.endpoint.as_str(),
            )),
        )
        .unwrap(),
    ));
    let app = router(state);
    let wav = voiceprompt_core::wav::to_wav(&vec![0; 320000]).unwrap();
    let mut input = request(
        "POST",
        "/v1/dictation/transcribe?language=cs&language=en",
        Body::from(wav.clone()),
    );
    input
        .headers_mut()
        .insert("Content-Type", HeaderValue::from_static("audio/wav"));
    let response = app.clone().oneshot(input).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Cache-Control"], "no-store");
    assert_eq!(
        json_body(response).await,
        serde_json::json!({"text":"synthetic dictation"})
    );
    let response = app
        .oneshot(request(
            "POST",
            "/v1/dictation/refine",
            Body::from(r#"{"text":"This is is a test.","previous_text":"synthetic prior"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        serde_json::json!({"text":"This is a test.", "edits":[{"original":" is is","replacement":" is"}]})
    );
    server.next().await;
    let input = server.next().await;
    assert_eq!(part(&input.body, "audio"), wav);
    let definition: serde_json::Value =
        serde_json::from_slice(part(&input.body, "definition")).unwrap();
    assert_eq!(definition["locales"], serde_json::json!(["en"]));
    server.finish().await;
}

type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn serve_ws(state: ApiState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    (endpoint, server)
}
async fn connect_ws(endpoint: &str, query: &str, authorized: bool) -> TestSocket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("{endpoint}/v1/dictation/stream{query}")
        .into_client_request()
        .unwrap();
    if authorized {
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_static("Bearer synthetic-token"),
        );
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .unwrap()
    .unwrap()
    .0
}
async fn ws_json(socket: &mut TestSocket) -> serde_json::Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn actual_websocket_route_preserves_auth_query_unavailable_and_capacity_rejection_frames() {
    use tokio_tungstenite::tungstenite::Message;
    let (endpoint, server) = serve_ws(fixture()).await;
    for (query, authorized, code, close_code) in [
        ("", false, "unauthorized", 4401),
        ("?start_byte=1", true, "invalid_request", 4400),
        ("?language=invalid", true, "invalid_request", 4400),
        ("", true, "unavailable", 4503),
    ] {
        let mut socket = connect_ws(&endpoint, query, authorized).await;
        assert_eq!(
            ws_json(&mut socket).await,
            serde_json::json!({"type":"error","code":code})
        );
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("Missing rejection close");
        };
        assert_eq!(u16::from(close.code), close_code);
    }
    server.abort();
    let mut state = fixture();
    state.streaming = Some(Arc::new(
        crate::streaming::MaiStreamingProvider::new(
            &state.services.config,
            Arc::new(crate::identity::ManagedIdentity::fixture(
                "http://127.0.0.1:9/token",
            )),
            "fixture-model",
        )
        .unwrap(),
    ));
    let _occupied = state
        .stream_slots
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    let (endpoint, server) = serve_ws(state).await;
    let mut socket = connect_ws(&endpoint, "", true).await;
    assert_eq!(ws_json(&mut socket).await["code"], "too_many_requests");
    server.abort();
}

#[tokio::test]
async fn actual_pcm_relay_orders_out_of_order_completion_and_done_at_absolute_cursor() {
    use crate::http_fixture::{config, serve, token};
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let resource: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let mut state = fixture();
    state.streaming = Some(Arc::new(
        crate::streaming::MaiStreamingProvider::new(
            &config(&resource),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    ));
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
        let mut captured = Vec::new();
        let mut commits = 0;
        while let Some(Ok(Message::Text(data))) = socket.next().await {
            let event: serde_json::Value = serde_json::from_str(&data).unwrap();
            match event["type"].as_str().unwrap() {
                "input_audio_buffer.append" => captured.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(event["audio"].as_str().unwrap())
                        .unwrap(),
                ),
                "input_audio_buffer.commit" => {
                    commits += 1;
                    socket.send(Message::Text(serde_json::json!({"type":"input_audio_buffer.committed","item_id":format!("item-{commits}")}).to_string().into())).await.unwrap();
                    if commits == 2 {
                        for (item, text) in [
                            ("item-2", "second confirmed"),
                            ("item-1", "first confirmed"),
                        ] {
                            socket.send(Message::Text(serde_json::json!({"type":"conversation.item.input_audio_transcription.completed","item_id":item,"transcript":text}).to_string().into())).await.unwrap();
                        }
                    }
                }
                _ => panic!("Unexpected upstream request"),
            }
        }
        assert_eq!(captured, [vec![1; 640], vec![2; 640]].concat());
    });
    let (endpoint, server) = serve_ws(state).await;
    let mut socket = connect_ws(&endpoint, "?language=cs&start_byte=1280", true).await;
    assert_eq!(
        ws_json(&mut socket).await,
        serde_json::json!({"type":"ready","protocol":1,"start_byte":1280})
    );
    socket
        .send(Message::Binary(vec![1; 640].into()))
        .await
        .unwrap();
    socket
        .send(Message::Text(r#"{"type":"commit"}"#.into()))
        .await
        .unwrap();
    socket
        .send(Message::Binary(vec![2; 640].into()))
        .await
        .unwrap();
    socket
        .send(Message::Text(r#"{"type":"finish"}"#.into()))
        .await
        .unwrap();
    assert_eq!(
        ws_json(&mut socket).await,
        serde_json::json!({"type":"confirmed","item_id":"item-1","text":"first confirmed","byte_end":1920})
    );
    assert_eq!(ws_json(&mut socket).await["byte_end"], 2560);
    assert_eq!(
        ws_json(&mut socket).await,
        serde_json::json!({"type":"done","byte_end":2560})
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
    server.abort();
}

#[tokio::test]
async fn missing_confirmation_times_out_at_thirty_seconds_without_done_or_provisional_paste() {
    use crate::http_fixture::{config, serve, token};
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let resource: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let mut state = fixture();
    state.streaming = Some(Arc::new(
        crate::streaming::MaiStreamingProvider::new(
            &config(&resource),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    ));
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
        while let Some(Ok(Message::Text(data))) = socket.next().await {
            let event: serde_json::Value = serde_json::from_str(&data).unwrap();
            if event["type"] == "input_audio_buffer.commit" {
                socket
                    .send(Message::Text(
                        r#"{"type":"input_audio_buffer.committed","item_id":"pending"}"#.into(),
                    ))
                    .await
                    .unwrap();
                socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"pending","delta":"provisional must not paste"}"#.into())).await.unwrap();
            }
        }
    });
    let (endpoint, server) = serve_ws(state).await;
    let mut socket = connect_ws(&endpoint, "", true).await;
    assert_eq!(ws_json(&mut socket).await["type"], "ready");
    socket
        .send(Message::Binary(vec![1; 640].into()))
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    socket
        .send(Message::Text(r#"{"type":"finish"}"#.into()))
        .await
        .unwrap();
    assert_eq!(ws_json(&mut socket).await["type"], "delta");
    let error = tokio::time::timeout(std::time::Duration::from_secs(35), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let error: serde_json::Value = serde_json::from_str(error.to_text().unwrap()).unwrap();
    assert_eq!(error["type"], "error");
    assert_eq!(error["code"], "stream_failed");
    assert!(started.elapsed() >= std::time::Duration::from_secs(30));
    assert!(started.elapsed() < std::time::Duration::from_secs(35));
    let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
        panic!("Missing failed-stream close");
    };
    assert_eq!(u16::from(close.code), 1011);
    tokio::time::timeout(std::time::Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while slots.available_permits() != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    identity.finish().await;
    server.abort();
}

#[tokio::test]
async fn server_shutdown_cancels_an_active_websocket_without_confirming_partial_audio_or_leaking_capacity()
 {
    use crate::http_fixture::{config, serve, token};
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let mut state = fixture();
    let shutdown = state.shutdown.clone();
    let slots = state.stream_slots.clone();
    state.streaming = Some(Arc::new(
        crate::streaming::MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    ));
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
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    let (endpoint, server) = serve_ws(state).await;
    let mut socket = connect_ws(&endpoint, "", true).await;
    assert_eq!(ws_json(&mut socket).await["type"], "ready");
    shutdown.send_replace(true);
    let message = ws_json(&mut socket).await;
    assert_eq!(message["type"], "error");
    assert_eq!(message["code"], "stream_failed");
    tokio::time::timeout(std::time::Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while slots.available_permits() != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    identity.finish().await;
    server.abort();
}
