use super::*;
use crate::ai::AzureAi;
use crate::gateway::WebPubSub;
use crate::http_fixture::{config, serve, token};
use crate::identity::ManagedIdentity;
use crate::ports::Queue;
use crate::services::ChunkUpload;
use base64::Engine;
use sha2::{Digest, Sha256};

async fn fixture(endpoint: &url::Url) -> (QueueWorker, String) {
    let mut services = crate::services::tests::fixture();
    let mut config = config(endpoint);
    config.max_transcribe_attempts = 2;
    services.config = Arc::new(config);
    let services = Arc::new(services);
    let (record, _) = services
        .create(
            serde_json::from_value(serde_json::json!({
                "client_recording_id":"fixture","refinement_enabled":false
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let identity = Arc::new(ManagedIdentity::fixture(endpoint.as_str()));
    let pipeline = Arc::new(Pipeline {
        services: services.clone(),
        ai: Arc::new(AzureAi::new(services.config.clone(), identity.clone()).unwrap()),
        realtime: Arc::new(WebPubSub::new(services.config.clone(), identity.clone()).unwrap()),
    });
    let streaming =
        Arc::new(MaiStreamingProvider::new(&services.config, identity, "fixture-model").unwrap());
    (
        QueueWorker {
            pipeline,
            streaming,
        },
        record.recording_id,
    )
}
async fn upload(worker: &QueueWorker, id: &str) {
    let wav = voiceprompt_core::wav::to_wav(&[1, 2, 3, 4]).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    worker
        .pipeline
        .services
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
}
#[tokio::test]
async fn actual_queue_worker_processes_legacy_then_finalize_and_replays_settled_messages_without_another_model_call()
 {
    let mut server = serve(vec![
        token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"synthetic final"}]}"#.into(),
        ),
        token(),
        (202, "".into()),
    ])
    .await;
    let (worker, id) = fixture(&server.endpoint).await;
    upload(&worker, &id).await;
    worker.pipeline.services.complete(&id, 1).await.unwrap();
    let (_stop, stopping) = watch::channel(false);
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    let record = worker.pipeline.services.recording(&id).await.unwrap();
    assert_eq!(record.state, RecordingState::Completed);
    assert!(record.stream_delivery_completed);
    assert_eq!(
        worker
            .pipeline
            .services
            .transcript(record.transcript_id.as_ref().unwrap())
            .await
            .unwrap()
            .body,
        "synthetic final"
    );
    worker
        .pipeline
        .services
        .queue
        .send(
            serde_json::json!({"type":"transcribe","recording_id":id,"index":0}),
            0,
        )
        .await
        .unwrap();
    worker
        .pipeline
        .services
        .queue
        .send(serde_json::json!({"type":"finalize","recording_id":id}), 0)
        .await
        .unwrap();
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(!worker.run_once(stopping).await.unwrap());
    for _ in 0..4 {
        server.next().await;
    }
    server.finish().await;
}
#[tokio::test]
async fn transient_model_failure_renews_visibility_then_exhaustion_durably_fails_and_settles() {
    let mut server = serve(vec![
        token(),
        (503, "private body".into()),
        (503, "private body".into()),
    ])
    .await;
    let (worker, id) = fixture(&server.endpoint).await;
    upload(&worker, &id).await;
    let (_stop, stopping) = watch::channel(false);
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    let services = &worker.pipeline.services;
    assert_ne!(
        services.recording(&id).await.unwrap().state,
        RecordingState::Failed
    );
    let Queue::Memory(memory) = &services.queue else {
        panic!("memory fixture");
    };
    {
        let mut messages = memory.messages.lock().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message.dequeue_count, 1);
        assert!(messages[0].visible_at > tokio::time::Instant::now());
        messages[0].visible_at = tokio::time::Instant::now();
    }
    worker.run_once(stopping).await.unwrap();
    let record = services.recording(&id).await.unwrap();
    assert_eq!(record.state, RecordingState::Failed);
    assert_eq!(
        record.failure_reason.as_deref(),
        Some("transcription_failed")
    );
    assert!(memory.messages.lock().await.is_empty());
    for _ in 0..3 {
        server.next().await;
    }
    server.finish().await;
}
#[tokio::test]
async fn malformed_work_is_explicitly_settled_and_receive_outages_remain_stop_responsive() {
    let endpoint: url::Url = "http://127.0.0.1:9/".parse().unwrap();
    let (worker, id) = fixture(&endpoint).await;
    worker
        .pipeline
        .services
        .queue
        .send(serde_json::json!({"type":"unknown","recording_id":id}), 0)
        .await
        .unwrap();
    let (stop, stopping) = watch::channel(false);
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    let Queue::Memory(memory) = &worker.pipeline.services.queue else {
        panic!("memory fixture");
    };
    assert!(memory.messages.lock().await.is_empty());
    memory
        .failed
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(worker.run_once(stopping.clone()).await.is_err());
    let worker = Arc::new(worker);
    let run = tokio::spawn(async move { worker.run(stopping).await });
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
#[test]
fn retry_backoff_is_bounded_even_for_large_delivery_counts() {
    assert_eq!(backoff(0, 30, 600), 30);
    assert_eq!(backoff(1, 30, 600), 30);
    assert_eq!(backoff(2, 30, 600), 60);
    assert_eq!(backoff(u32::MAX, 30, 600), 600);
}

#[tokio::test]
async fn already_completed_notification_retries_beyond_inference_attempt_limit_instead_of_dropping_delivery()
 {
    let mut server = serve(vec![
        token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"synthetic final"}]}"#.into(),
        ),
        token(),
        (503, "outage".into()),
        (503, "outage".into()),
        (202, "".into()),
    ])
    .await;
    let (mut worker, id) = fixture(&server.endpoint).await;
    let mut config = (*worker.pipeline.services.config).clone();
    config.max_refine_attempts = 1;
    Arc::get_mut(&mut Arc::get_mut(&mut worker.pipeline).unwrap().services)
        .unwrap()
        .config = Arc::new(config);
    upload(&worker, &id).await;
    worker.pipeline.services.complete(&id, 1).await.unwrap();
    let (_stop, stopping) = watch::channel(false);
    worker.run_once(stopping.clone()).await.unwrap();
    worker.run_once(stopping.clone()).await.unwrap();
    let services = &worker.pipeline.services;
    let Queue::Memory(memory) = &services.queue else {
        panic!("memory fixture");
    };
    for attempt in 0..2 {
        assert!(
            !services
                .recording(&id)
                .await
                .unwrap()
                .stream_delivery_completed
        );
        {
            let mut messages = memory.messages.lock().await;
            let entry = messages
                .iter_mut()
                .find(|entry| {
                    entry.message.content["type"] == "finalize" && entry.message.dequeue_count > 0
                })
                .unwrap();
            entry.visible_at = tokio::time::Instant::now();
        }
        worker.run_once(stopping.clone()).await.unwrap();
        if attempt == 0 {
            assert!(
                !services
                    .recording(&id)
                    .await
                    .unwrap()
                    .stream_delivery_completed
            );
        }
    }
    let record = services.recording(&id).await.unwrap();
    assert_eq!(record.state, RecordingState::Completed);
    assert!(record.stream_delivery_completed);
    for _ in 0..6 {
        server.next().await;
    }
    server.finish().await;
}
