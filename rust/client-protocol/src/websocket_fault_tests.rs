use super::*;
use tokio::net::TcpListener;

fn access(port: u16, lifetime: time::Duration) -> NegotiateResponse {
    NegotiateResponse {
        url: format!("ws://127.0.0.1:{port}/"),
        hub: "fixture".into(),
        group: "fixture".into(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(2) + lifetime,
    }
}

fn message(id: &str) -> Message {
    Message::Text(
        serde_json::json!({
            "event":"transcript.completed","event_id":id,"transcript_id":"t","recording_id":"r",
        })
        .to_string()
        .into(),
    )
}

#[tokio::test]
async fn renewal_is_absolute_even_when_the_handshake_consumes_the_remaining_lifetime() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, tests::accept_protocol)
            .await
            .unwrap();
        socket.send(message("expired")).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    let access = access(port, time::Duration::milliseconds(75));
    let (_control, mut control) = watch::channel(Control::default());
    let (_auth, mut auth) = watch::channel(AuthState::SignedIn);
    let (state, _) = watch::channel(RealtimeState::Stopped);
    let (sender, mut events) = mpsc::channel(1);
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        receive_connection(&access, &mut control, &mut auth, &state, &sender, true),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(end, ConnectionEnd::Renew);
    assert_eq!(*state.borrow(), RealtimeState::Stopped);
    assert!(events.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn stalled_write_is_cancellable_and_has_a_real_send_deadline() {
    use tokio_tungstenite::tungstenite::protocol::Role;
    for cancel in [true, false] {
        let (stream, _not_reading) = tokio::io::duplex(64);
        let mut socket =
            tokio_tungstenite::WebSocketStream::from_raw_socket(stream, Role::Client, None).await;
        let (control, mut commands) = watch::channel(Control::default());
        let (_auth, mut auth) = watch::channel(AuthState::SignedIn);
        let renewal = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(renewal);
        let sending = send_controlled(
            &mut socket,
            Message::Binary(vec![0; 128 * 1024].into()),
            &mut commands,
            &mut auth,
            renewal.as_mut(),
        );
        let (result, ()) = tokio::join!(sending, async {
            if cancel {
                tokio::time::sleep(Duration::from_millis(20)).await;
                control.send_modify(|command| command.stopped = true);
            }
        });
        if cancel {
            assert_eq!(result.unwrap(), Some(ConnectionEnd::ControlChanged));
        } else {
            assert_eq!(result.unwrap_err(), RealtimeError::Send);
        }
    }
}

#[tokio::test]
async fn oversized_frame_is_rejected_before_an_event_can_be_queued() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, tests::accept_protocol)
            .await
            .unwrap();
        let _ = socket
            .send(Message::Text("x".repeat(1024 * 1024 + 1).into()))
            .await;
    });
    let access = access(port, time::Duration::minutes(5));
    let (_control, mut commands) = watch::channel(Control::default());
    let (_auth, mut auth) = watch::channel(AuthState::SignedIn);
    let (state, _) = watch::channel(RealtimeState::Stopped);
    let (sender, mut events) = mpsc::channel(1);
    assert_eq!(
        receive_connection(&access, &mut commands, &mut auth, &state, &sender, true)
            .await
            .unwrap_err(),
        RealtimeError::Receive
    );
    assert!(events.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn full_event_channel_does_not_block_stop_or_signout() {
    for sign_out in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, tests::accept_protocol)
                .await
                .unwrap();
            socket.send(message("blocked")).await.unwrap();
            sent.send(()).unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        let access = access(port, time::Duration::minutes(5));
        let (control, mut commands) = watch::channel(Control::default());
        let (auth, mut auth_state) = watch::channel(AuthState::SignedIn);
        let (state, _) = watch::channel(RealtimeState::Stopped);
        let (sender, mut events) = mpsc::channel(1);
        sender
            .send(
                Frame::parse(
                    br#"{"event":"transcript.completed","event_id":"prefill","transcript_id":"t"}"#,
                )
                .unwrap()
                .completed()
                .unwrap()
                .unwrap(),
            )
            .await
            .unwrap();
        let connection = tokio::spawn(async move {
            receive_connection(
                &access,
                &mut commands,
                &mut auth_state,
                &state,
                &sender,
                true,
            )
            .await
        });
        ready.await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        if sign_out {
            auth.send_replace(AuthState::SignedOut);
        } else {
            control.send_modify(|command| command.stopped = true);
        }
        let end = tokio::time::timeout(Duration::from_secs(5), connection)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(end, ConnectionEnd::ControlChanged);
        assert_eq!(events.recv().await.unwrap().event_id, "prefill");
        assert!(events.try_recv().is_err());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn malformed_frames_are_rejected_without_losing_the_next_valid_event() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, tests::accept_protocol)
            .await
            .unwrap();
        for text in [
            "{garbage",
            r#"{"type":"message","dataType":"text","data":"invalid-json"}"#,
            r#"{"type":"ack","ackId":1,"success":true}"#,
        ] {
            socket.send(Message::Text(text.into())).await.unwrap();
        }
        socket.send(message("valid")).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    let access = access(port, time::Duration::minutes(5));
    let (control, mut commands) = watch::channel(Control::default());
    let (_auth, mut auth) = watch::channel(AuthState::SignedIn);
    let (state, _) = watch::channel(RealtimeState::Stopped);
    let (sender, mut events) = mpsc::channel(1);
    let connection = tokio::spawn(async move {
        receive_connection(&access, &mut commands, &mut auth, &state, &sender, true).await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap()
            .event_id,
        "valid"
    );
    control.send_modify(|command| command.stopped = true);
    assert_eq!(
        connection.await.unwrap().unwrap(),
        ConnectionEnd::ControlChanged
    );
    server.await.unwrap();
}

#[tokio::test]
async fn closed_consumer_is_an_explicit_terminal_failure() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, tests::accept_protocol)
            .await
            .unwrap();
        socket.send(message("valid")).await.unwrap();
    });
    let access = access(port, time::Duration::minutes(5));
    let (_control, mut commands) = watch::channel(Control::default());
    let (_auth, mut auth) = watch::channel(AuthState::SignedIn);
    let (state, _) = watch::channel(RealtimeState::Stopped);
    let (sender, events) = mpsc::channel(1);
    drop(events);
    assert_eq!(
        receive_connection(&access, &mut commands, &mut auth, &state, &sender, true)
            .await
            .unwrap_err(),
        RealtimeError::ConsumerClosed
    );
    server.await.unwrap();
}
