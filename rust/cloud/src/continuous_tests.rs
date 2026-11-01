use super::*;
use crate::http_fixture::{config, serve, token};
use crate::services::ChunkUpload;
use crate::stream_worker::StreamLeases;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

async fn fixture() -> (Arc<Services>, String, Arc<QueueReceipt>) {
    let mut services = crate::services::tests::fixture();
    Arc::make_mut(&mut services.config).recording_stream_heartbeat_seconds = 0.02;
    let services = Arc::new(services);
    let (record, _) = services.create(serde_json::from_value(serde_json::json!({
        "client_recording_id":"fixture","transcription_mode":"streaming","audio_layout":"contiguous",
        "refinement_enabled":false,
    })).unwrap()).await.unwrap();
    services
        .queue
        .send(
            serde_json::json!({"type":"stream","recording_id":record.recording_id}),
            0,
        )
        .await
        .unwrap();
    let message = services.queue.receive(300).await.unwrap().remove(0);
    (
        services.clone(),
        record.recording_id,
        Arc::new(QueueReceipt::new(services.queue.clone(), message)),
    )
}
async fn upload(services: &Services, id: &str, index: u32, pcm: Vec<u8>) {
    let samples = pcm.len() / 2;
    let wav = voiceprompt_core::wav::to_wav(&pcm).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    services
        .upload(
            id,
            ChunkUpload {
                index,
                wav,
                checksum,
                duration_ms: None,
                overlap_ms: Some(0),
                started_at: None,
                start_sample: Some(u64::from(index) * 160000),
                sample_count: Some(samples as u64),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_continuous_attempt_replays_from_zero_in_one_stream_then_publishes_only_durable_final_text()
 {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = Arc::new(
        MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    );
    let (services, id, receipt) = fixture().await;
    let first = vec![1; 320000];
    let second = vec![2; 640];
    upload(&services, &id, 0, first.clone()).await;
    upload(&services, &id, 1, second.clone()).await;
    services.complete(&id, 2).await.unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
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
                if event["type"] == "input_audio_buffer.append" {
                    captured.extend(
                        base64::engine::general_purpose::STANDARD
                            .decode(event["audio"].as_str().unwrap())
                            .unwrap(),
                    );
                    if attempt == 0 {
                        socket
                            .send(Message::Text(
                                r#"{"type":"error","message":"synthetic fault"}"#.into(),
                            ))
                            .await
                            .unwrap();
                    }
                } else if event["type"] == "input_audio_buffer.commit" {
                    commits += 1;
                    socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"synthetic final text"}"#.into())).await.unwrap();
                } else {
                    panic!("Unexpected model request");
                }
            }
            if attempt == 0 {
                assert!(captured.len() >= 640 && captured.len() < 320000);
            } else {
                assert_eq!(captured, [first.clone(), second.clone()].concat());
                assert_eq!(
                    commits, 1,
                    "There must be no model reset or intermediate commit between durable segments."
                );
            }
        }
    });
    let (_stop, stopping) = watch::channel(false);
    assert!(
        transcribe_recording(
            services.clone(),
            provider.clone(),
            id.clone(),
            receipt.clone(),
            stopping.clone()
        )
        .await
        .is_err()
    );
    let failed = services.recording(&id).await.unwrap();
    assert!(!failed.stream_asr_ready);
    assert!(failed.stream_lease_id.is_none());
    assert_eq!(failed.stream_error.as_deref(), Some("stream_failed"));
    assert_eq!(services.chunks(&id).await.unwrap().len(), 2);
    transcribe_recording(services.clone(), provider, id.clone(), receipt, stopping)
        .await
        .unwrap();
    let ready = services.recording(&id).await.unwrap();
    assert!(ready.stream_asr_ready && ready.finalize_enqueued);
    assert_eq!(ready.stream_attempt, 2);
    assert_eq!(ready.streamed_samples, 160320);
    assert!(ready.stream_lease_id.is_none() && ready.stream_error.is_none());
    assert!(
        ready
            .stream_raw_path
            .as_deref()
            .unwrap()
            .starts_with(&format!("stream/{id}/"))
    );
    assert_eq!(
        services
            .blobs
            .get(
                &services.config.raw_container,
                ready.stream_raw_path.as_ref().unwrap(),
                MAX_TEXT_BYTES
            )
            .await
            .unwrap(),
        b"synthetic final text"
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
}

#[tokio::test]
async fn worker_shutdown_waiting_for_uploads_releases_both_model_and_recording_without_a_ready_result()
 {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = Arc::new(
        MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    );
    let (services, id, receipt) = fixture().await;
    let connected = Arc::new(tokio::sync::Notify::new());
    let ready = connected.clone();
    let server = tokio::spawn(async move {
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
        ready.notify_one();
        assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
    });
    let (stop, stopping) = watch::channel(false);
    let worker_services = services.clone();
    let worker_id = id.clone();
    let worker = tokio::spawn(async move {
        transcribe_recording(worker_services, provider, worker_id, receipt, stopping).await
    });
    connected.notified().await;
    stop.send_replace(true);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(StreamWorkError::Pending(_))
    ));
    let record = services.recording(&id).await.unwrap();
    assert!(!record.stream_asr_ready && record.stream_raw_path.is_none());
    assert!(record.stream_lease_id.is_none());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
}

#[tokio::test]
async fn lease_takeover_stops_the_old_stream_and_late_release_cannot_clear_the_new_owner() {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = Arc::new(
        MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    );
    let (services, id, receipt) = fixture().await;
    upload(&services, &id, 0, vec![1; 320000]).await;
    services.complete(&id, 1).await.unwrap();
    let audio_seen = Arc::new(tokio::sync::Notify::new());
    let seen = audio_seen.clone();
    let server = tokio::spawn(async move {
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
            assert_ne!(event["type"], "input_audio_buffer.commit");
            seen.notify_one();
        }
    });
    let (_stop, stopping) = watch::channel(false);
    let worker_services = services.clone();
    let worker_id = id.clone();
    let worker = tokio::spawn(async move {
        transcribe_recording(worker_services, provider, worker_id, receipt, stopping).await
    });
    audio_seen.notified().await;
    loop {
        let mut record = services.recording(&id).await.unwrap();
        record.stream_lease_expires_at = Some(now() - time::Duration::seconds(1));
        match services.save_recording(&record).await {
            Ok(_) => break,
            Err(crate::storage::StorageError::Conflict) => continue,
            Err(error) => panic!("Synthetic expiry failed: {error}"),
        }
    }
    StreamLeases::new(services.clone())
        .claim(&id, "successor")
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let record = services.recording(&id).await.unwrap();
    assert_eq!(record.stream_lease_id.as_deref(), Some("successor"));
    assert!(!record.stream_asr_ready && record.stream_raw_path.is_none());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
}

#[tokio::test]
async fn legacy_overlap_windows_feed_one_continuous_model_stream_without_duplicate_samples_or_silence()
 {
    let identity = serve(vec![token()]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let provider = Arc::new(
        MaiStreamingProvider::new(
            &config(&endpoint),
            Arc::new(crate::identity::ManagedIdentity::fixture(
                identity.endpoint.as_str(),
            )),
            "fixture-model",
        )
        .unwrap(),
    );
    let mut services = crate::services::tests::fixture();
    Arc::make_mut(&mut services.config).recording_stream_replay_speed = 8.0;
    let services = Arc::new(services);
    let (record, _) = services.create(serde_json::from_value(serde_json::json!({
        "client_recording_id":"legacy","transcription_mode":"streaming","audio_layout":"legacy_overlap",
        "refinement_enabled":false,
    })).unwrap()).await.unwrap();
    let id = record.recording_id;
    let samples = |start: i32, count: i32| {
        (start..start + count)
            .flat_map(|sample| ((sample % 32767) as i16).to_le_bytes())
            .collect::<Vec<_>>()
    };
    upload(&services, &id, 0, samples(0, 480000)).await;
    upload(&services, &id, 1, samples(456000, 480000)).await;
    services.complete(&id, 2).await.unwrap();
    let receipt = Arc::new(QueueReceipt::new(
        services.queue.clone(),
        services.queue.receive(300).await.unwrap().remove(0),
    ));
    let server = tokio::spawn(async move {
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
            if event["type"] == "input_audio_buffer.append" {
                captured.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(event["audio"].as_str().unwrap())
                        .unwrap(),
                );
            } else if event["type"] == "input_audio_buffer.commit" {
                commits += 1;
                socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"legacy continuous final"}"#.into())).await.unwrap();
            } else {
                panic!("Unexpected model event");
            }
        }
        assert_eq!(captured, samples(0, 936000));
        assert_eq!(commits, 1);
    });
    let (_stop, stopping) = watch::channel(false);
    transcribe_recording(services.clone(), provider, id.clone(), receipt, stopping)
        .await
        .unwrap();
    let record = services.recording(&id).await.unwrap();
    assert_eq!(record.streamed_samples, 936000);
    assert!(record.stream_asr_ready && record.stream_lease_id.is_none());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    identity.finish().await;
}
