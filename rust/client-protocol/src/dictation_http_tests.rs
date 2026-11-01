use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use voiceprompt_core::wav::to_wav;

struct Request {
    headers: String,
    body: Vec<u8>,
}

fn endpoint(
    replies: Vec<(u16, String, bool)>,
) -> (url::Url, std::thread::JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, reply, truncated) in replies {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < until =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    error => panic!("HTTP fixture accept failed: {error:?}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut buffer = [0; 4096];
            let header_end = loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                data.extend_from_slice(&buffer[..count]);
                if let Some(end) = data.windows(4).position(|value| value == b"\r\n\r\n") {
                    break end + 4;
                }
                assert!(data.len() <= 64 * 1024);
            };
            let headers = String::from_utf8(data[..header_end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length <= 2 * 1024 * 1024);
            let mut body = data[header_end..].to_vec();
            while body.len() < length {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                body.extend_from_slice(&buffer[..count]);
            }
            requests.push(Request { headers, body });
            let length = reply.len() + if truncated { 100 } else { 0 };
            stream.write_all(format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{reply}",
            ).as_bytes()).unwrap();
        }
        requests
    });
    (
        url::Url::parse(&format!("http://127.0.0.1:{}/", address.port())).unwrap(),
        server,
    )
}

#[tokio::test]
async fn transcription_sends_exact_wav_mime_and_id_token_with_one_transient_retry() {
    let (base, server) = endpoint(vec![
        (503, "{}".into(), false),
        (200, r#"{"text":"synthetic"}"#.into(), false),
    ]);
    let (client, bearer) = crate::api::tests::make_client(base).await;
    let wav = to_wav(&[1; 6400]).unwrap();
    assert_eq!(
        client.transcribe_dictation(&wav, "cs").await.unwrap(),
        "synthetic"
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(
            request
                .headers
                .starts_with("POST /v1/dictation/transcribe?language=cs ")
        );
        assert!(
            request
                .headers
                .to_ascii_lowercase()
                .contains("content-type: audio/wav")
        );
        assert!(request.headers.to_ascii_lowercase().contains(&format!(
            "authorization: bearer {}",
            bearer.to_ascii_lowercase()
        )));
        assert_eq!(request.body, wav);
    }
}

#[tokio::test]
async fn interrupted_success_body_retries_and_malformed_success_is_not_a_transcript() {
    let (base, server) = endpoint(vec![
        (200, r#"{"text":"incomplete"}"#.into(), true),
        (200, r#"{"text":"synthetic complete"}"#.into(), false),
    ]);
    let (client, _) = crate::api::tests::make_client(base).await;
    assert_eq!(
        client
            .transcribe_dictation(&to_wav(&[1; 640]).unwrap(), "auto")
            .await
            .unwrap(),
        "synthetic complete"
    );
    assert_eq!(server.join().unwrap().len(), 2);
    let (base, server) = endpoint(vec![(200, r#"{"text":null}"#.into(), false)]);
    let (client, _) = crate::api::tests::make_client(base).await;
    assert!(
        client
            .transcribe_dictation(&to_wav(&[1; 640]).unwrap(), "auto")
            .await
            .unwrap_err()
            .response_invalid
    );
    server.join().unwrap();
}

#[tokio::test]
async fn polishing_has_no_transient_retry_and_rejects_malformed_edits() {
    let (base, server) = endpoint(vec![(503, "{}".into(), false)]);
    let (client, _) = crate::api::tests::make_client(base).await;
    assert_eq!(
        client
            .refine_dictation("synthetic", "context")
            .await
            .unwrap_err()
            .kind,
        ApiErrorKind::Retryable
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap()["previous_text"],
        "context"
    );
    let (base, server) = endpoint(vec![(
        200,
        r#"{"text":"synthetic","edits":[{"original":"","replacement":"x"}]}"#.into(),
        false,
    )]);
    let (client, _) = crate::api::tests::make_client(base).await;
    assert!(
        client
            .refine_dictation("synthetic", "")
            .await
            .unwrap_err()
            .response_invalid
    );
    server.join().unwrap();
    assert_eq!(
        client.refine_dictation("", "").await.unwrap_err().kind,
        ApiErrorKind::BadRequest
    );
}

#[tokio::test]
async fn archive_uses_fixed_original_completion_and_accepts_python_microsecond_precision() {
    let completed = OffsetDateTime::from_unix_timestamp_nanos(2_000_000_000_000_000_100).unwrap();
    let mut state = crate::recovery::RecoveryManifest::create(
        "auto".into(),
        "synthetic context".into(),
        true,
        crate::recovery::utc_ticks(completed).unwrap(),
    )
    .unwrap();
    let wav = to_wav(&[1; 640]).unwrap();
    state = state
        .checkpoint(
            &[],
            640,
            &[crate::recovery::AudioCheckpoint {
                wav: &wav,
                overlaps_previous: false,
                end_byte: 640,
            }],
            true,
            state.created_at_ticks,
        )
        .unwrap();
    state
        .confirm(
            640,
            "synthetic original".into(),
            Vec::new(),
            false,
            None,
            state.created_at_ticks,
        )
        .unwrap();
    state
        .freeze_audit(
            "synthetic final".into(),
            state.created_at_ticks,
            true,
            state.created_at_ticks,
        )
        .unwrap();
    let id = format!(
        "{}-{}-{}-{}-{}",
        &state.id[..8],
        &state.id[8..12],
        &state.id[12..16],
        &state.id[16..20],
        &state.id[20..]
    );
    let rounded = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let summary = serde_json::json!({
        "transcript_id":id,"recording_id":id,"preview":"synthetic","character_count":15,
        "completed_at":rounded.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "expires_at":(rounded + time::Duration::hours(48)).format(&time::format_description::well_known::Rfc3339).unwrap(),
        "source":"windows_dictation",
    }).to_string();
    let (base, server) = endpoint(vec![(200, summary, false)]);
    let (client, _) = crate::api::tests::make_client(base).await;
    assert_eq!(
        client
            .archive_dictation(&state)
            .await
            .unwrap()
            .transcript_id,
        id
    );
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .headers
            .starts_with(&format!("PUT /v1/dictation/transcripts/{} ", state.id))
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["raw_text"], "synthetic original");
    assert_eq!(body["text"], "synthetic final");
    assert_eq!(body["transcribe_model"], "MAI-Transcribe-2-Streaming");
    assert_eq!(
        body["completed_at"],
        completed
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
}
