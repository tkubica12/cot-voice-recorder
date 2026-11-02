use super::*;
use std::collections::BTreeMap;
use voiceprompt_core::wav::to_wav;

pub(crate) fn fixture() -> Services {
    let values: BTreeMap<String, String> = [
        ("VR_ENVIRONMENT", "test"),
        ("VR_GOOGLE_ALLOWED_AUDIENCES", "fixture"),
        ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid"),
        ("VR_STORAGE_ACCOUNT_NAME", "fixture123"),
        (
            "VR_FOUNDRY_ENDPOINT",
            "https://fixture.cognitiveservices.azure.com/",
        ),
        (
            "VR_SPEECH_ENDPOINT",
            "https://fixture.cognitiveservices.azure.com/",
        ),
        (
            "VR_WEBPUBSUB_ENDPOINT",
            "https://fixture.webpubsub.azure.com/",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    Services::new(
        Arc::new(Config::parse(&values).unwrap()),
        Repository::memory(),
        Blobs::memory(),
        Queue::memory(),
    )
    .unwrap()
}
fn create(client: &str, streaming: bool) -> models::CreateRecording {
    serde_json::from_value(serde_json::json!({
        "client_recording_id":client,
        "transcription_mode":if streaming {"streaming"} else {"chunked"},
        "audio_layout":if streaming {"contiguous"} else {"legacy_overlap"},
        "refinement_enabled":!streaming,
    }))
    .unwrap()
}
fn upload(index: u32) -> ChunkUpload {
    ChunkUpload {
        index,
        wav: to_wav(&vec![0; 320_000]).unwrap(),
        checksum: format!("synthetic-{index}"),
        duration_ms: Some(10000),
        overlap_ms: Some(0),
        started_at: None,
        start_sample: Some(u64::from(index) * 160_000),
        sample_count: Some(160_000),
    }
}

#[tokio::test]
async fn create_upload_complete_is_idempotent_and_conflicts_do_not_replace_published_audio() {
    let services = fixture();
    let (record, created) = services.create(create("client", true)).await.unwrap();
    assert!(created);
    let (same, created) = services.create(create("client", false)).await.unwrap();
    assert!(!created);
    assert_eq!(record.recording_id, same.recording_id);
    assert_eq!(same.transcription_mode, "streaming");
    let id = &record.recording_id;
    assert_eq!(services.upload(id, upload(0)).await.unwrap().1, 202);
    assert_eq!(services.upload(id, upload(0)).await.unwrap().1, 200);
    let mut different = upload(0);
    different.checksum = "different".into();
    assert_eq!(
        services.upload(id, different).await.err().unwrap().code,
        "chunk-conflict"
    );
    assert_eq!(services.complete(id, 1).await.unwrap().1, 202);
    assert_eq!(services.complete(id, 1).await.unwrap().1, 200);
    assert_eq!(
        services.complete(id, 2).await.err().unwrap().code,
        "complete-conflict"
    );
    assert_eq!(
        services.recording(id).await.unwrap().state,
        RecordingState::Transcribing
    );
    assert_eq!(
        services
            .wire_recording(&services.recording(id).await.unwrap())
            .await
            .unwrap()
            .progress
            .received_chunk_count,
        1
    );
}

#[tokio::test]
async fn failed_queue_reservation_is_released_and_complete_replay_repairs_work() {
    let services = fixture();
    let (record, _) = services.create(create("client", false)).await.unwrap();
    let id = &record.recording_id;
    services.upload(id, upload(0)).await.unwrap();
    let mut chunk = services.chunks(id).await.unwrap().remove(0);
    chunk.state = ChunkState::Transcribed;
    chunk.text = Some("synthetic transcript".into());
    services
        .repository
        .replace(&services.config.chunks_table, chunk.entity().unwrap())
        .await
        .unwrap();
    let Queue::Memory(queue) = &services.queue else {
        panic!("memory fixture")
    };
    queue
        .failed
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(services.complete(id, 1).await.is_err());
    assert!(!services.recording(id).await.unwrap().finalize_enqueued);
    queue
        .failed
        .store(false, std::sync::atomic::Ordering::Release);
    services.complete(id, 1).await.unwrap();
    assert!(services.recording(id).await.unwrap().finalize_enqueued);
    assert!(
        queue
            .messages
            .lock()
            .await
            .iter()
            .any(|entry| entry.message.content["type"] == "finalize" && entry.delay == 0)
    );
}

#[tokio::test]
async fn an_existing_dangling_client_index_repairs_the_original_recording_id_not_a_new_candidate() {
    let services = fixture();
    let index = Entity {
        values: serde_json::json!({
            "PartitionKey":"crid","RowKey":"client","recording_id":"original-fixture-id",
        })
        .as_object()
        .unwrap()
        .clone(),
        etag: String::new(),
    };
    services
        .repository
        .insert(&services.config.recordings_table, index)
        .await
        .unwrap();
    let (record, created) = services.create(create("client", false)).await.unwrap();
    assert!(created);
    assert_eq!(record.recording_id, "original-fixture-id");
}

#[tokio::test]
async fn frozen_audit_roundtrips_and_retries_cannot_extend_expiry_or_emit_work_notifications() {
    let services = fixture();
    let completed = now() - time::Duration::minutes(1);
    let id = "0123456789abcdef0123456789abcdef";
    let data = models::ArchiveDictation {
        text: "synthetic final".into(),
        raw_text: "synthetic original".into(),
        language: "auto".into(),
        completed_at: completed,
        polished: false,
        transcribe_model: "MAI-Transcribe-2-Streaming".into(),
    };
    let first = services.archive(id, data.clone()).await.unwrap();
    let second = services.archive(id, data.clone()).await.unwrap();
    assert_eq!(first.completed_at, second.completed_at);
    assert_eq!(
        first.expires_at - first.completed_at,
        time::Duration::hours(48)
    );
    let body = services.transcript(&first.transcript_id).await.unwrap();
    assert_eq!(body.body, data.text);
    assert_eq!(body.raw_body.as_deref(), Some("synthetic original"));
    let mut conflicting = data.clone();
    conflicting.text = "conflicting".into();
    assert_eq!(
        services
            .archive(id, conflicting)
            .await
            .err()
            .unwrap()
            .status,
        409
    );
    let Queue::Memory(queue) = &services.queue else {
        panic!("memory fixture")
    };
    assert!(queue.messages.lock().await.is_empty());
}

#[test]
fn python_cursor_and_envelope_dates_keep_fixed_microsecond_format() {
    let date = OffsetDateTime::parse(
        "2026-11-01T10:00:00.123400Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert_eq!(
        python_datetime(date, false),
        "2026-11-01T10:00:00.123400+00:00"
    );
    assert_eq!(python_datetime(date, true), "2026-11-01T10:00:00.123400Z");
    assert_eq!(preview(&"x ".repeat(100), 10), "x x x x x\u{2026}");
}

#[tokio::test]
async fn configured_preview_limit_reaches_persisted_audit_summary() {
    let mut services = fixture();
    Arc::make_mut(&mut services.config).preview_max_chars = 8;
    let payload:models::ArchiveDictation=serde_json::from_value(serde_json::json!({
        "text":"synthetic long text","raw_text":"synthetic raw","completed_at":python_datetime(now(),true)
    })).unwrap();
    let result = services
        .archive("0123456789abcdef0123456789abcdef", payload)
        .await
        .unwrap();
    assert_eq!(result.preview.chars().count(), 8);
    assert_eq!(result.preview, "synthet\u{2026}");
}
