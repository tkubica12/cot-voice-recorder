use super::*;
use crate::services::ChunkUpload;
use base64::Engine;
use sha2::{Digest, Sha256};

async fn fixture() -> (Arc<Services>, String) {
    let services = Arc::new(crate::services::tests::fixture());
    let (record, _) = services.create(serde_json::from_value(serde_json::json!({
        "client_recording_id":"fixture","transcription_mode":"streaming","audio_layout":"contiguous",
    })).unwrap()).await.unwrap();
    (services, record.recording_id)
}
fn upload(index: u32, pcm: Vec<u8>) -> ChunkUpload {
    let wav = voiceprompt_core::wav::to_wav(&pcm).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    ChunkUpload {
        index,
        wav,
        checksum,
        duration_ms: None,
        overlap_ms: None,
        started_at: None,
        start_sample: Some(u64::from(index) * 160000),
        sample_count: Some((pcm.len() / 2) as u64),
    }
}
#[tokio::test]
async fn only_one_concurrent_owner_can_claim_and_expired_owner_cannot_publish_or_release_a_successor()
 {
    let (services, id) = fixture().await;
    let leases = Arc::new(StreamLeases::new(services.clone()));
    let mut handles = Vec::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    for index in 0..16 {
        let leases = leases.clone();
        let id = id.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            leases.claim(&id, &format!("owner-{index}")).await.unwrap()
        }));
    }
    let mut claimed = Vec::new();
    for handle in handles {
        if let Some(record) = handle.await.unwrap() {
            claimed.push(record);
        }
    }
    assert_eq!(claimed.len(), 1);
    let old = claimed.pop().unwrap();
    assert_eq!(old.stream_attempt, 1);
    let owner = old.stream_lease_id.clone().unwrap();
    let mut expired = services.recording(&id).await.unwrap();
    expired.stream_lease_expires_at = Some(now() - time::Duration::seconds(1));
    services.save_recording(&expired).await.unwrap();
    assert!(matches!(
        leases.renew(&id, &owner).await,
        Err(StreamWorkError::Pending(_))
    ));
    let successor = leases.claim(&id, "successor").await.unwrap().unwrap();
    assert_eq!(successor.stream_attempt, 2);
    assert!(matches!(
        services.save_recording(&old).await,
        Err(StorageError::Conflict)
    ));
    assert!(matches!(
        leases
            .update_owned(&id, &owner, |record| record.stream_asr_ready = true)
            .await,
        Err(StreamWorkError::Pending(_))
    ));
    leases
        .release(&id, &owner, Some("late_failure"))
        .await
        .unwrap();
    let record = services.recording(&id).await.unwrap();
    assert_eq!(record.stream_lease_id.as_deref(), Some("successor"));
    assert!(!record.stream_asr_ready);
    assert!(record.stream_error.is_none());
}
#[tokio::test]
async fn lease_renewal_preserves_progress_and_release_keeps_ready_publication() {
    let (services, id) = fixture().await;
    let leases = StreamLeases::new(services.clone());
    let record = leases.claim(&id, "owner").await.unwrap().unwrap();
    leases
        .update_owned(&id, "owner", |record| {
            record.streamed_samples = 160000;
            record.stream_preview = "synthetic".into();
        })
        .await
        .unwrap();
    let renewed = leases.renew(&id, "owner").await.unwrap();
    assert!(renewed.stream_lease_expires_at >= record.stream_lease_expires_at);
    assert_eq!(renewed.streamed_samples, 160000);
    leases
        .update_owned(&id, "owner", |record| {
            record.stream_asr_ready = true;
            record.stream_raw_path = Some("stream/fixture/owner.txt".into());
        })
        .await
        .unwrap();
    leases
        .release(&id, "owner", Some("late_failure"))
        .await
        .unwrap();
    let record = services.recording(&id).await.unwrap();
    assert!(record.stream_lease_id.is_none() && record.stream_lease_expires_at.is_none());
    assert!(record.stream_asr_ready && record.stream_error.is_none());
}
#[tokio::test]
async fn durable_contiguous_replay_preserves_every_sample_and_checksum_failure_cannot_change_cursor()
 {
    let (services, id) = fixture().await;
    let first = vec![1; 320000];
    let second = vec![2; 640];
    services
        .upload(&id, upload(0, first.clone()))
        .await
        .unwrap();
    services
        .upload(&id, upload(1, second.clone()))
        .await
        .unwrap();
    assert_eq!(
        pcm_for_chunk(&services, &id, 0, 0).await.unwrap().unwrap(),
        first
    );
    assert_eq!(
        pcm_for_chunk(&services, &id, 1, 160000)
            .await
            .unwrap()
            .unwrap(),
        second
    );
    assert!(
        pcm_for_chunk(&services, &id, 2, 160320)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        pcm_for_chunk(&services, &id, 1, 159999).await,
        Err(StreamWorkError::Terminal(_))
    ));
    let chunk = services.chunks(&id).await.unwrap().remove(0);
    services
        .blobs
        .put(
            &services.config.audio_container,
            chunk.blob_path.as_ref().unwrap(),
            vec![0; 44],
            "audio/wav",
        )
        .await
        .unwrap();
    assert!(matches!(
        pcm_for_chunk(&services, &id, 0, 0).await,
        Err(StreamWorkError::Terminal(_))
    ));
}
#[tokio::test]
async fn legacy_overlap_replay_discards_only_duplicate_sample_positions_not_recognized_words() {
    let services = crate::services::tests::fixture();
    let (record, _) = services.create(serde_json::from_value(serde_json::json!({
        "client_recording_id":"legacy","transcription_mode":"streaming","audio_layout":"legacy_overlap",
    })).unwrap()).await.unwrap();
    let id = &record.recording_id;
    let pcm: Vec<_> = (0..960000).map(|index| (index % 251) as u8).collect();
    let mut chunk = upload(1, pcm.clone());
    chunk.start_sample = None;
    chunk.sample_count = None;
    services.upload(id, chunk).await.unwrap();
    assert_eq!(
        pcm_for_chunk(&services, id, 1, 480000)
            .await
            .unwrap()
            .unwrap(),
        pcm[48000..]
    );
    assert!(matches!(
        pcm_for_chunk(&services, id, 1, 936000).await,
        Err(StreamWorkError::Terminal(_))
    ));
}
