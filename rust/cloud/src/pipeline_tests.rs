use super::*;
use crate::http_fixture::{config, serve, token};
use crate::identity::ManagedIdentity;
use crate::services::ChunkUpload;
use base64::Engine;
use sha2::{Digest, Sha256};

async fn fixture(endpoint: &url::Url, refined: bool) -> (Pipeline, String, QueueReceipt) {
    let mut services = crate::services::tests::fixture();
    services.config = Arc::new(config(endpoint));
    let services = Arc::new(services);
    let (record, _) = services
        .create(
            serde_json::from_value(serde_json::json!({
                "client_recording_id":"fixture","refinement_enabled":refined
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    services
        .queue
        .send(
            serde_json::json!({"type":"finalize","recording_id":record.recording_id}),
            0,
        )
        .await
        .unwrap();
    let receipt = QueueReceipt::new(
        services.queue.clone(),
        services.queue.receive(300).await.unwrap().remove(0),
    );
    let credential = Arc::new(ManagedIdentity::fixture(endpoint.as_str()));
    let pipeline = Pipeline {
        services: services.clone(),
        ai: Arc::new(AzureAi::new(services.config.clone(), credential.clone()).unwrap()),
        realtime: Arc::new(WebPubSub::new(services.config.clone(), credential).unwrap()),
    };
    (pipeline, record.recording_id, receipt)
}
async fn upload(pipeline: &Pipeline, id: &str, index: u32) {
    let wav = voiceprompt_core::wav::to_wav(&[1, 2, 3, 4]).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    pipeline
        .services
        .upload(
            id,
            ChunkUpload {
                index,
                wav,
                checksum,
                duration_ms: None,
                overlap_ms: None,
                started_at: None,
                start_sample: None,
                sample_count: None,
            },
        )
        .await
        .unwrap();
}
async fn transcribed(pipeline: &Pipeline, id: &str, index: u32, text: &str) {
    upload(pipeline, id, index).await;
    let mut chunk = pipeline.chunk(id, index).await.unwrap().unwrap();
    chunk.state = ChunkState::Transcribed;
    chunk.text = Some(text.into());
    pipeline
        .services
        .repository
        .replace(
            &pipeline.services.config.chunks_table,
            chunk.entity().unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_transcription_refinement_and_failed_notification_retry_keep_canonical_body_ids_and_expiry()
 {
    let mut server = serve(vec![
        token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"synthetic repeated seam"}]}"#.into(),
        ),
        (
            200,
            r#"{"combinedPhrases":[{"text":"repeated seam continues"}]}"#.into(),
        ),
        (
            200,
            r#"{"choices":[{"message":{"content":"synthetic refined final"}}]}"#.into(),
        ),
        token(),
        (503, "private provider error".into()),
        (202, "".into()),
    ])
    .await;
    let (pipeline, id, receipt) = fixture(&server.endpoint, true).await;
    upload(&pipeline, &id, 0).await;
    upload(&pipeline, &id, 1).await;
    pipeline.services.complete(&id, 2).await.unwrap();
    let (_stop, stopping) = watch::channel(false);
    for index in 0..2 {
        pipeline
            .transcribe_chunk(&id, index, &receipt, stopping.clone())
            .await
            .unwrap();
        let chunk = pipeline.chunk(&id, index).await.unwrap().unwrap();
        assert_eq!(chunk.state, ChunkState::Transcribed);
        assert!(matches!(
            pipeline
                .services
                .blobs
                .get(
                    &pipeline.services.config.audio_container,
                    chunk.blob_path.as_ref().unwrap(),
                    1024
                )
                .await,
            Err(StorageError::NotFound)
        ));
    }
    assert!(
        pipeline
            .finalize(&id, &receipt, stopping.clone())
            .await
            .is_err()
    );
    let record = pipeline.services.recording(&id).await.unwrap();
    assert_eq!(record.state, RecordingState::Completed);
    assert!(!record.stream_delivery_completed);
    let transcript_id = crate::pipeline_text::transcript_id(&id);
    let first = pipeline.metadata(&transcript_id).await.unwrap().unwrap();
    assert_eq!(first.body_path, format!("{transcript_id}.txt"));
    assert_eq!(
        pipeline
            .services
            .blobs
            .get(
                &pipeline.services.config.raw_container,
                &format!("{id}.txt"),
                4096
            )
            .await
            .unwrap(),
        b"synthetic repeated seam continues"
    );
    pipeline
        .finalize(&id, &receipt, stopping.clone())
        .await
        .unwrap();
    pipeline.finalize(&id, &receipt, stopping).await.unwrap();
    assert!(
        pipeline
            .services
            .recording(&id)
            .await
            .unwrap()
            .stream_delivery_completed
    );
    let final_meta = pipeline.metadata(&transcript_id).await.unwrap().unwrap();
    assert_eq!(first.completed_at, final_meta.completed_at);
    assert_eq!(first.expires_at, final_meta.expires_at);
    let full = pipeline.services.transcript(&transcript_id).await.unwrap();
    assert_eq!(full.body, "synthetic refined final");
    assert_eq!(full.character_count, 23);
    let mut requests = Vec::new();
    for _ in 0..7 {
        requests.push(server.next().await);
    }
    let events: Vec<serde_json::Value> = requests
        .iter()
        .filter(|request| request.target.path().ends_with("/:send"))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], events[1]);
    assert_eq!(events[0]["event_id"], crate::pipeline_text::event_id(&id));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.target.path().ends_with("/chat/completions"))
            .count(),
        1
    );
    server.finish().await;
}

#[tokio::test]
async fn interrupted_legacy_model_work_keeps_durable_audio_and_accepted_state_for_retry() {
    let mut server = serve(vec![
        token(),
        (503, "private body".into()),
        (
            200,
            r#"{"combinedPhrases":[{"text":"retried text"}]}"#.into(),
        ),
    ])
    .await;
    let (pipeline, id, receipt) = fixture(&server.endpoint, false).await;
    upload(&pipeline, &id, 0).await;
    let (_stop, stopping) = watch::channel(false);
    assert!(
        pipeline
            .transcribe_chunk(&id, 0, &receipt, stopping.clone())
            .await
            .is_err()
    );
    let chunk = pipeline.chunk(&id, 0).await.unwrap().unwrap();
    assert_eq!(chunk.state, ChunkState::Accepted);
    assert!(chunk.text.is_none());
    assert!(
        pipeline
            .services
            .blobs
            .get(
                &pipeline.services.config.audio_container,
                chunk.blob_path.as_ref().unwrap(),
                1024
            )
            .await
            .is_ok()
    );
    pipeline
        .transcribe_chunk(&id, 0, &receipt, stopping)
        .await
        .unwrap();
    assert_eq!(
        pipeline
            .chunk(&id, 0)
            .await
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some("retried text")
    );
    for _ in 0..3 {
        server.next().await;
    }
    server.finish().await;
}

#[tokio::test]
async fn crash_after_immutable_body_write_resumes_without_a_second_model_call_or_replacing_canonical_text()
 {
    let mut server = serve(vec![token(), (202, "".into())]).await;
    let (pipeline, id, receipt) = fixture(&server.endpoint, true).await;
    transcribed(&pipeline, &id, 0, "synthetic raw").await;
    pipeline.services.complete(&id, 1).await.unwrap();
    let transcript_id = crate::pipeline_text::transcript_id(&id);
    pipeline
        .services
        .blobs
        .put_if_absent(
            &pipeline.services.config.transcript_container,
            &format!("{transcript_id}.txt"),
            b"original canonical winner".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    let (_stop, stopping) = watch::channel(false);
    pipeline.finalize(&id, &receipt, stopping).await.unwrap();
    let full = pipeline.services.transcript(&transcript_id).await.unwrap();
    assert_eq!(full.body, "original canonical winner");
    assert_eq!(full.preview, "original canonical winner");
    assert_eq!(full.character_count, 25);
    server.next().await;
    let request = server.next().await;
    assert!(request.target.path().ends_with("/:send"));
    server.finish().await;
}

#[tokio::test]
async fn missing_or_failed_chunks_never_publish_final_text_and_grace_expiry_is_explicit() {
    let endpoint: url::Url = "http://127.0.0.1:9/".parse().unwrap();
    let (pipeline, id, receipt) = fixture(&endpoint, false).await;
    pipeline.services.complete(&id, 1).await.unwrap();
    let (_stop, stopping) = watch::channel(false);
    assert!(
        pipeline
            .finalize(&id, &receipt, stopping.clone())
            .await
            .is_err()
    );
    assert!(
        pipeline
            .services
            .recording(&id)
            .await
            .unwrap()
            .transcript_id
            .is_none()
    );
    let mut record = pipeline.services.recording(&id).await.unwrap();
    record.complete_requested_at = Some(now() - time::Duration::seconds(901));
    pipeline.services.save_recording(&record).await.unwrap();
    pipeline.finalize(&id, &receipt, stopping).await.unwrap();
    let record = pipeline.services.recording(&id).await.unwrap();
    assert_eq!(record.state, RecordingState::Failed);
    assert_eq!(record.failure_reason.as_deref(), Some("missing_chunks"));
}

#[tokio::test]
async fn completed_stream_notification_failure_keeps_durable_source_until_acknowledged_retry() {
    let mut server = serve(vec![
        token(),
        (503, "synthetic outage".into()),
        (202, "".into()),
    ])
    .await;
    let (pipeline, id, receipt) = fixture(&server.endpoint, false).await;
    let mut record = pipeline.services.recording(&id).await.unwrap();
    record.transcription_mode = "streaming".into();
    record.audio_layout = "contiguous".into();
    pipeline.services.save_recording(&record).await.unwrap();
    let wav = voiceprompt_core::wav::to_wav(&[1, 2, 3, 4]).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    pipeline
        .services
        .upload(
            &id,
            ChunkUpload {
                index: 0,
                wav,
                checksum,
                duration_ms: None,
                overlap_ms: Some(0),
                started_at: None,
                start_sample: Some(0),
                sample_count: Some(2),
            },
        )
        .await
        .unwrap();
    pipeline.services.complete(&id, 1).await.unwrap();
    let raw = format!("stream/{id}/synthetic-owner.txt");
    pipeline
        .services
        .blobs
        .put(
            &pipeline.services.config.raw_container,
            &raw,
            b"synthetic streaming final".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    let mut record = pipeline.services.recording(&id).await.unwrap();
    record.stream_asr_ready = true;
    record.stream_raw_path = Some(raw.clone());
    pipeline.services.save_recording(&record).await.unwrap();
    let chunk = pipeline.chunk(&id, 0).await.unwrap().unwrap();
    let path = chunk.blob_path.unwrap();
    let (_stop, stopping) = watch::channel(false);
    assert!(
        pipeline
            .finalize(&id, &receipt, stopping.clone())
            .await
            .is_err()
    );
    assert!(
        pipeline
            .services
            .blobs
            .get(&pipeline.services.config.audio_container, &path, 1024)
            .await
            .is_ok()
    );
    assert!(
        pipeline
            .services
            .blobs
            .get(&pipeline.services.config.raw_container, &raw, 1024)
            .await
            .is_ok()
    );
    pipeline.finalize(&id, &receipt, stopping).await.unwrap();
    assert!(matches!(
        pipeline
            .services
            .blobs
            .get(&pipeline.services.config.audio_container, &path, 1024)
            .await,
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        pipeline
            .services
            .blobs
            .get(&pipeline.services.config.raw_container, &raw, 1024)
            .await,
        Err(StorageError::NotFound)
    ));
    for _ in 0..3 {
        server.next().await;
    }
    server.finish().await;
}
