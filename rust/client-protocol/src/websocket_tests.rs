use super::*;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

// Tungstenite's callback fixes ErrorResponse to a non-boxed HTTP response.
#[allow(clippy::result_large_err)]
fn accept_protocol(request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    assert_eq!(request.headers()["Sec-WebSocket-Protocol"], SUBPROTOCOL);
    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    Ok(response)
}

async fn exercise_connection(sign_out: bool) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accept_protocol)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"system","event":"connected"}"#.into(),
            ))
            .await
            .unwrap();
        socket.send(Message::Text(r#"{"type":"message","dataType":"json","data":{"event":"transcript.completed","event_id":"e","transcript_id":"t","recording_id":"r","completed_at":"2026-11-01T12:00:00Z","preview":"synthetic"}} "#.into())).await.unwrap();
        while let Some(message) = socket.next().await {
            if matches!(message.unwrap(), Message::Close(_)) {
                break;
            }
        }
    });
    let access = NegotiateResponse {
        url: format!("ws://127.0.0.1:{port}/"),
        hub: "fixture".into(),
        group: "fixture".into(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
    };
    let (control, mut commands) = watch::channel(Control::default());
    let (auth, mut auth_state) = watch::channel(AuthState::SignedIn);
    let (state, _) = watch::channel(RealtimeState::Stopped);
    let (events, mut receiver) = mpsc::channel(2);
    let connection = tokio::spawn(async move {
        receive_connection(
            &access,
            &mut commands,
            &mut auth_state,
            &state,
            &events,
            true,
        )
        .await
    });
    let event = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.event_id, "e");
    assert_eq!(event.transcript_id, "t");
    if sign_out {
        auth.send_replace(AuthState::SignedOut);
    } else {
        control.send_modify(|command| command.stopped = true);
    }
    assert_eq!(
        connection.await.unwrap().unwrap(),
        ConnectionEnd::ControlChanged
    );
    server.await.unwrap();
}

#[tokio::test]
async fn real_websocket_negotiates_protocol_delivers_events_and_stops() {
    exercise_connection(false).await;
}

#[tokio::test]
async fn signout_cancels_an_active_websocket_before_more_delivery() {
    exercise_connection(true).await;
}

#[test]
fn backoff_stays_between_half_ceiling_and_cap() {
    for attempt in [1, 2, 3, 10, u32::MAX] {
        let delay = reconnect_delay(attempt).unwrap();
        let ceiling = (2f64.powi(attempt.saturating_sub(1).min(30) as i32)).min(60.0);
        assert!(delay.as_secs_f64() >= ceiling * 0.5);
        assert!(delay.as_secs_f64() <= ceiling);
    }
}
