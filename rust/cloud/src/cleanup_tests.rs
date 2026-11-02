use super::*;
use crate::services::ChunkUpload;
use base64::Engine;
use sha2::{Digest, Sha256};

async fn recording(services: &Services, client: &str) -> String {
    services
        .create(serde_json::from_value(serde_json::json!({"client_recording_id":client})).unwrap())
        .await
        .unwrap()
        .0
        .recording_id
}
async fn expired(services: &Services, id: &str) {
    let mut record = services.recording(id).await.unwrap();
    record.created_at = now() - time::Duration::hours(49);
    record.updated_at = record.created_at;
    services.save_recording(&record).await.unwrap();
}
async fn audio(services: &Services, id: &str) -> String {
    let wav = voiceprompt_core::wav::to_wav(&[1, 2, 3, 4]).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    services
        .upload(
            id,
            ChunkUpload {
                index: 0,
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
    services
        .chunks(id)
        .await
        .unwrap()
        .remove(0)
        .blob_path
        .unwrap()
}
#[tokio::test]
async fn expired_recording_is_conditionally_fenced_then_audio_chunks_raw_index_and_metadata_are_removed_idempotently()
 {
    let services = crate::services::tests::fixture();
    let id = recording(&services, "expired").await;
    let path = audio(&services, &id).await;
    expired(&services, &id).await;
    services
        .blobs
        .put(
            &services.config.raw_container,
            &format!("{id}.txt"),
            b"synthetic raw".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    let result = run(&services).await.unwrap();
    assert_eq!(result.recordings_deleted, 1);
    assert_eq!(result.audio_blobs_deleted, 1);
    assert_eq!(services.recording(&id).await.err().unwrap().status, 404);
    assert!(services.chunks(&id).await.unwrap().is_empty());
    assert!(
        services
            .repository
            .get(&services.config.recordings_table, "crid", "expired")
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        services
            .blobs
            .get(&services.config.audio_container, &path, 1024)
            .await,
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        services
            .blobs
            .get(&services.config.raw_container, &format!("{id}.txt"), 1024)
            .await,
        Err(StorageError::NotFound)
    ));
    assert_eq!(run(&services).await.unwrap().recordings_deleted, 0);
}
#[tokio::test]
async fn active_continuous_audio_and_finalizer_leases_block_the_entire_recording_cleanup() {
    let services = crate::services::tests::fixture();
    let continuous = recording(&services, "continuous").await;
    audio(&services, &continuous).await;
    expired(&services, &continuous).await;
    let mut record = services.recording(&continuous).await.unwrap();
    record.stream_lease_id = Some("active-owner".into());
    record.stream_lease_expires_at = Some(now() + time::Duration::seconds(120));
    services.save_recording(&record).await.unwrap();
    let legacy = recording(&services, "legacy").await;
    let path = audio(&services, &legacy).await;
    expired(&services, &legacy).await;
    let audio_lease = BlobLease::existing(
        services.blobs.clone(),
        services.config.audio_container.clone(),
        path.clone(),
    )
    .await
    .unwrap();
    let finalizing = recording(&services, "finalizing").await;
    audio(&services, &finalizing).await;
    expired(&services, &finalizing).await;
    let finalizer = BlobLease::acquire(
        services.blobs.clone(),
        services.config.raw_container.clone(),
        format!("finalize/{finalizing}.lock"),
    )
    .await
    .unwrap();
    let result = run(&services).await.unwrap();
    assert_eq!(result.recordings_deleted, 0);
    for id in [&continuous, &legacy, &finalizing] {
        assert!(services.recording(id).await.is_ok());
        assert_eq!(services.chunks(id).await.unwrap().len(), 1);
    }
    audio_lease.release().await.unwrap();
    finalizer.release().await.unwrap();
    assert_eq!(run(&services).await.unwrap().recordings_deleted, 2);
    assert!(services.recording(&continuous).await.is_ok());
}
#[tokio::test]
async fn expired_audit_and_orphans_obey_fixed_expiry_while_active_publication_and_fresh_unknown_data_are_preserved()
 {
    let services = crate::services::tests::fixture();
    let payload:crate::models::ArchiveDictation=serde_json::from_value(serde_json::json!({
        "text":"synthetic final","raw_text":"synthetic raw","completed_at":crate::services::python_datetime(now(),true)
    })).unwrap();
    let mut transcript = services
        .archive("0123456789abcdef0123456789abcdef", payload.clone())
        .await
        .unwrap();
    transcript.expires_at = now() - time::Duration::seconds(1);
    services
        .repository
        .replace(
            &services.config.transcripts_table,
            transcript.entity().unwrap(),
        )
        .await
        .unwrap();
    let active = BlobLease::existing(
        services.blobs.clone(),
        services.config.transcript_container.clone(),
        transcript.body_path.clone(),
    )
    .await
    .unwrap();
    assert_eq!(run(&services).await.unwrap().transcripts_deleted, 0);
    active.release().await.unwrap();
    let mut orphan = payload;
    orphan.completed_at = now() - time::Duration::hours(49);
    let orphan_path = "windows-dictation/orphan/digest.json";
    services
        .blobs
        .put(
            &services.config.transcript_container,
            orphan_path,
            serde_json::to_vec(&orphan).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    for name in [
        "windows-dictation/old/invalid.json",
        "windows-dictation/fresh/invalid.json",
    ] {
        services
            .blobs
            .put(
                &services.config.transcript_container,
                name,
                b"synthetic invalid".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
    }
    services
        .blobs
        .age_fixture(
            &services.config.transcript_container,
            "windows-dictation/old/invalid.json",
            now() - time::Duration::hours(49),
        )
        .await;
    assert_eq!(run(&services).await.unwrap().transcripts_deleted, 1);
    for name in [
        transcript.body_path.as_str(),
        orphan_path,
        "windows-dictation/old/invalid.json",
    ] {
        assert!(matches!(
            services
                .blobs
                .get(&services.config.transcript_container, name, 4096)
                .await,
            Err(StorageError::NotFound)
        ));
    }
    assert!(
        services
            .blobs
            .get(
                &services.config.transcript_container,
                "windows-dictation/fresh/invalid.json",
                4096
            )
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn fresh_unpublished_audio_is_not_mistaken_for_an_orphan_and_failed_stream_source_keeps_retention()
 {
    let services = crate::services::tests::fixture();
    for path in ["missing/fresh.wav", "missing/old.wav"] {
        services
            .blobs
            .put(
                &services.config.audio_container,
                path,
                vec![0; 44],
                "audio/wav",
            )
            .await
            .unwrap();
    }
    services
        .blobs
        .age_fixture(
            &services.config.audio_container,
            "missing/old.wav",
            now() - time::Duration::hours(49),
        )
        .await;
    let id = recording(&services, "failed-stream").await;
    let path = audio(&services, &id).await;
    let mut record = services.recording(&id).await.unwrap();
    record.transcription_mode = "streaming".into();
    record.state = crate::domain::RecordingState::Failed;
    services.save_recording(&record).await.unwrap();
    let result = run(&services).await.unwrap();
    assert_eq!(result.audio_blobs_deleted, 1);
    assert!(
        services
            .blobs
            .get(&services.config.audio_container, "missing/fresh.wav", 1024)
            .await
            .is_ok()
    );
    assert!(
        services
            .blobs
            .get(&services.config.audio_container, &path, 1024)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn expired_transcript_delete_failure_preserves_its_metadata_for_a_retry_instead_of_leaving_an_untracked_body()
 {
    let services = crate::services::tests::fixture();
    let payload:crate::models::ArchiveDictation=serde_json::from_value(serde_json::json!({
        "text":"synthetic final","raw_text":"synthetic raw","completed_at":crate::services::python_datetime(now(),true)
    })).unwrap();
    let mut transcript = services
        .archive("0123456789abcdef0123456789abcdef", payload)
        .await
        .unwrap();
    transcript.expires_at = now() - time::Duration::seconds(1);
    services
        .repository
        .replace(
            &services.config.transcripts_table,
            transcript.entity().unwrap(),
        )
        .await
        .unwrap();
    services
        .blobs
        .fail_delete_fixture(&transcript.body_path)
        .await;
    assert!(run(&services).await.is_err());
    assert!(
        services
            .repository
            .get(
                &services.config.transcripts_table,
                "transcript",
                &transcript.transcript_id
            )
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        services
            .blobs
            .get(
                &services.config.transcript_container,
                &transcript.body_path,
                4096
            )
            .await
            .is_ok()
    );
    assert_eq!(run(&services).await.unwrap().transcripts_deleted, 1);
    assert!(
        services
            .repository
            .get(
                &services.config.transcripts_table,
                "transcript",
                &transcript.transcript_id
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn partially_failed_recording_cleanup_remains_durably_discoverable_and_resumes_after_ownership_expires()
 {
    let services = crate::services::tests::fixture();
    let id = recording(&services, "partial-cleanup").await;
    let path = audio(&services, &id).await;
    expired(&services, &id).await;
    services.blobs.fail_delete_fixture(&path).await;
    assert!(run(&services).await.is_err());
    let mut record = services.recording(&id).await.unwrap();
    assert_eq!(record.state, crate::domain::RecordingState::Failed);
    assert!(
        record
            .stream_lease_id
            .as_ref()
            .unwrap()
            .starts_with("cleanup-")
    );
    assert!(record.updated_at < now() - time::Duration::hours(48));
    assert_eq!(services.chunks(&id).await.unwrap().len(), 1);
    record.stream_lease_expires_at = Some(now() - time::Duration::seconds(1));
    services.save_recording(&record).await.unwrap();
    assert_eq!(run(&services).await.unwrap().recordings_deleted, 1);
    assert_eq!(services.recording(&id).await.err().unwrap().status, 404);
    assert!(services.chunks(&id).await.unwrap().is_empty());
}

#[tokio::test]
async fn old_unpublished_plain_transcript_is_removed_only_when_no_metadata_or_active_publisher_owns_it()
 {
    let services = crate::services::tests::fixture();
    let unpublished = "00000000-0000-4000-8000-000000000001.txt";
    let active = "00000000-0000-4000-8000-000000000002.txt";
    let fresh = "00000000-0000-4000-8000-000000000003.txt";
    for path in [unpublished, active, fresh, "unexpected.txt"] {
        services
            .blobs
            .put(
                &services.config.transcript_container,
                path,
                b"synthetic body".to_vec(),
                "text/plain",
            )
            .await
            .unwrap();
        if path != fresh {
            services
                .blobs
                .age_fixture(
                    &services.config.transcript_container,
                    path,
                    now() - time::Duration::hours(49),
                )
                .await;
        }
    }
    let publisher = BlobLease::existing(
        services.blobs.clone(),
        services.config.transcript_container.clone(),
        active.into(),
    )
    .await
    .unwrap();
    run(&services).await.unwrap();
    assert!(matches!(
        services
            .blobs
            .get(&services.config.transcript_container, unpublished, 4096)
            .await,
        Err(StorageError::NotFound)
    ));
    assert!(
        services
            .blobs
            .get(&services.config.transcript_container, active, 4096)
            .await
            .is_ok()
    );
    assert!(
        services
            .blobs
            .get(&services.config.transcript_container, fresh, 4096)
            .await
            .is_ok()
    );
    publisher.release().await.unwrap();
    run(&services).await.unwrap();
    assert!(matches!(
        services
            .blobs
            .get(&services.config.transcript_container, active, 4096)
            .await,
        Err(StorageError::NotFound)
    ));
    assert!(
        services
            .blobs
            .get(
                &services.config.transcript_container,
                "unexpected.txt",
                4096
            )
            .await
            .is_ok()
    );
}
