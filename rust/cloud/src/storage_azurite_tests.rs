use super::*;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

async fn worker_and_cleanup(storage: Arc<AzureStorage>) -> (String, String) {
    use crate::http_fixture::{config, serve, token};
    use crate::ports::{Blobs, Queue};
    use crate::services::{ChunkUpload, Services};
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let mut provider = serve(vec![
        token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"synthetic Azure worker final"}]}"#.into(),
        ),
        token(),
        (202, "".into()),
    ])
    .await;
    let config = Arc::new(config(&provider.endpoint));
    let services = Arc::new(
        Services::new(
            config.clone(),
            crate::repository::Repository::Azure(storage.clone()),
            Blobs::Azure(storage.clone()),
            Queue::Azure {
                storage: storage.clone(),
                name: config.queue.clone(),
            },
        )
        .unwrap(),
    );
    let identity = Arc::new(ManagedIdentity::fixture(provider.endpoint.as_str()));
    let worker = crate::worker::QueueWorker {
        pipeline: Arc::new(crate::pipeline::Pipeline {
            services: services.clone(),
            ai: Arc::new(crate::ai::AzureAi::new(config.clone(), identity.clone()).unwrap()),
            realtime: Arc::new(
                crate::gateway::WebPubSub::new(config.clone(), identity.clone()).unwrap(),
            ),
        }),
        streaming: Arc::new(
            crate::streaming::MaiStreamingProvider::new(&config, identity, "synthetic-model")
                .unwrap(),
        ),
    };
    let (record, created) = services
        .create(
            serde_json::from_value(json!({
                "client_recording_id":"actual-https-emulator-legacy", "refinement_enabled":false,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(created);
    let wav = voiceprompt_core::wav::to_wav(&[1, 0, 2, 0, 3, 0, 4, 0]).unwrap();
    let checksum = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
    );
    services
        .upload(
            &record.recording_id,
            ChunkUpload {
                index: 0,
                wav: wav.clone(),
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
    services.complete(&record.recording_id, 1).await.unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    let completed = services.recording(&record.recording_id).await.unwrap();
    assert_eq!(completed.state, crate::domain::RecordingState::Completed);
    assert!(completed.stream_delivery_completed);
    let transcript_id = completed.transcript_id.unwrap();
    assert_eq!(
        services.transcript(&transcript_id).await.unwrap().body,
        "synthetic Azure worker final"
    );
    assert!(matches!(
        storage
            .get_blob(
                &config.audio_container,
                &format!("{}/0.wav", record.recording_id),
                1024
            )
            .await,
        Err(StorageError::NotFound)
    ));
    services
        .queue
        .send(
            json!({"type":"transcribe","recording_id":record.recording_id,"index":0}),
            0,
        )
        .await
        .unwrap();
    services
        .queue
        .send(
            json!({"type":"finalize","recording_id":record.recording_id}),
            0,
        )
        .await
        .unwrap();
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    assert!(!worker.run_once(stopping).await.unwrap());
    provider.next().await;
    let speech = provider.next().await;
    assert_eq!(crate::http_fixture::part(&speech.body, "audio"), wav);
    provider.next().await;
    let event = provider.next().await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&event.body).unwrap()["event"],
        "transcript.completed"
    );
    provider.finish().await;

    let original = services
        .repository
        .get(&config.transcripts_table, "transcript", &transcript_id)
        .await
        .unwrap()
        .unwrap();
    let mut expired = crate::domain::Transcript::from_entity(&original).unwrap();
    expired.transcript_id = crate::services::uuid().unwrap();
    expired.recording_id = expired.transcript_id.clone();
    expired.body_path = format!("{}.txt", expired.transcript_id);
    expired.completed_at = crate::services::now() - time::Duration::hours(49);
    expired.expires_at = expired.completed_at + time::Duration::hours(48);
    expired.etag.clear();
    services
        .blobs
        .put(
            &config.transcript_container,
            &expired.body_path,
            b"synthetic Azure worker final".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    services
        .repository
        .insert(&config.transcripts_table, expired.entity().unwrap())
        .await
        .unwrap();
    let owner = crate::services::uuid().unwrap();
    storage
        .acquire_blob_lease(&config.transcript_container, &expired.body_path, &owner)
        .await
        .unwrap();
    let protected = crate::cleanup::run(&services).await.unwrap();
    assert_eq!(protected.transcripts_deleted, 0);
    assert!(
        services
            .repository
            .get(
                &config.transcripts_table,
                "transcript",
                &expired.transcript_id
            )
            .await
            .unwrap()
            .is_some()
    );
    storage
        .release_blob_lease(&config.transcript_container, &expired.body_path, &owner)
        .await
        .unwrap();
    let cleaned = crate::cleanup::run(&services).await.unwrap();
    assert_eq!(cleaned.transcripts_deleted, 1);
    assert!(
        services
            .repository
            .get(
                &config.transcripts_table,
                "transcript",
                &expired.transcript_id
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        storage
            .get_blob(&config.transcript_container, &expired.body_path, 1024)
            .await,
        Err(StorageError::NotFound)
    ));
    assert_eq!(
        services.transcript(&transcript_id).await.unwrap().body,
        "synthetic Azure worker final"
    );
    (record.recording_id, transcript_id)
}

async fn continuous_worker(storage: Arc<AzureStorage>) -> (String, String) {
    use crate::http_fixture::{config, serve, token};
    use crate::ports::{Blobs, Queue};
    use crate::services::{ChunkUpload, Services};
    use base64::Engine;
    use futures_util::{SinkExt, StreamExt};
    use sha2::{Digest, Sha256};
    use tokio_tungstenite::tungstenite::Message;

    let identity_server = serve(vec![token(), token()]).await;
    let mut notifications = serve(vec![(202, "".into())]).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let mut options = config(&endpoint);
    options.webpubsub = notifications.endpoint.clone();
    let config = Arc::new(options);
    let services = Arc::new(
        Services::new(
            config.clone(),
            crate::repository::Repository::Azure(storage.clone()),
            Blobs::Azure(storage.clone()),
            Queue::Azure {
                storage: storage.clone(),
                name: config.queue.clone(),
            },
        )
        .unwrap(),
    );
    let identity = Arc::new(ManagedIdentity::fixture(identity_server.endpoint.as_str()));
    let worker = crate::worker::QueueWorker {
        pipeline: Arc::new(crate::pipeline::Pipeline {
            services: services.clone(),
            ai: Arc::new(crate::ai::AzureAi::new(config.clone(), identity.clone()).unwrap()),
            realtime: Arc::new(
                crate::gateway::WebPubSub::new(config.clone(), identity.clone()).unwrap(),
            ),
        }),
        streaming: Arc::new(
            crate::streaming::MaiStreamingProvider::new(&config, identity, "synthetic-model")
                .unwrap(),
        ),
    };
    let (record, created) = services
        .create(
            serde_json::from_value(json!({
                "client_recording_id":"actual-https-emulator-continuous",
                "transcription_mode":"streaming", "audio_layout":"contiguous",
                "refinement_enabled":false,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(created);
    let segments = [vec![1; 320000], vec![2; 640]];
    for (index, pcm) in segments.iter().enumerate() {
        let wav = voiceprompt_core::wav::to_wav(pcm).unwrap();
        let checksum = format!(
            "sha-256=:{}:",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&wav))
        );
        services
            .upload(
                &record.recording_id,
                ChunkUpload {
                    index: index as u32,
                    wav,
                    checksum,
                    duration_ms: None,
                    overlap_ms: Some(0),
                    started_at: None,
                    start_sample: Some(index as u64 * 160000),
                    sample_count: Some(pcm.len() as u64 / 2),
                },
            )
            .await
            .unwrap();
    }
    services.complete(&record.recording_id, 2).await.unwrap();
    let expected = segments.concat();
    let model = tokio::spawn(async move {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(15), listener.accept())
            .await
            .unwrap()
            .unwrap();
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
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Text(data) => {
                    let event: serde_json::Value = serde_json::from_str(&data).unwrap();
                    match event["type"].as_str().unwrap() {
                        "input_audio_buffer.append" => {
                            captured.extend(
                                base64::engine::general_purpose::STANDARD
                                    .decode(event["audio"].as_str().unwrap())
                                    .unwrap(),
                            );
                            assert!(captured.len() <= expected.len());
                        }
                        "input_audio_buffer.commit" => {
                            commits += 1;
                            socket.send(Message::Text(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"synthetic Azure continuous final"}"#.into())).await.unwrap();
                        }
                        _ => panic!(
                            "Continuous model request must not reset between durable segments."
                        ),
                    }
                }
                Message::Close(_) => break,
                _ => panic!("Unexpected continuous model frame."),
            }
        }
        assert_eq!(commits, 1);
        assert_eq!(
            captured, expected,
            "No missing, duplicated or inserted PCM samples."
        );
    });
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    assert!(worker.run_once(stopping.clone()).await.unwrap());
    let ready = services.recording(&record.recording_id).await.unwrap();
    assert!(ready.stream_asr_ready);
    assert_eq!(ready.stream_attempt, 1);
    assert_eq!(ready.streamed_samples, 160320);
    assert!(ready.stream_lease_id.is_none());
    for index in 0..2 {
        assert!(
            storage
                .get_blob(
                    &config.audio_container,
                    &format!("{}/{index}.wav", record.recording_id),
                    1024 * 1024
                )
                .await
                .is_ok()
        );
    }
    for _ in 0..8 {
        if services
            .recording(&record.recording_id)
            .await
            .unwrap()
            .stream_delivery_completed
        {
            break;
        }
        assert!(worker.run_once(stopping.clone()).await.unwrap());
    }
    let completed = services.recording(&record.recording_id).await.unwrap();
    assert_eq!(completed.state, crate::domain::RecordingState::Completed);
    assert!(completed.stream_delivery_completed && completed.stream_error.is_none());
    let transcript_id = completed.transcript_id.unwrap();
    assert_eq!(
        services.transcript(&transcript_id).await.unwrap().body,
        "synthetic Azure continuous final"
    );
    for index in 0..2 {
        assert!(matches!(
            storage
                .get_blob(
                    &config.audio_container,
                    &format!("{}/{index}.wav", record.recording_id),
                    1024 * 1024
                )
                .await,
            Err(StorageError::NotFound)
        ));
    }
    tokio::time::timeout(Duration::from_secs(5), model)
        .await
        .unwrap()
        .unwrap();
    let event = notifications.next().await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&event.body).unwrap()["event"],
        "transcript.completed"
    );
    notifications.finish().await;
    identity_server.finish().await;
    (record.recording_id, transcript_id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    scope: String,
    blob: String,
    queue: String,
    table: String,
}

fn endpoint(uri: &url::Url, scheme: &str, path: &str) {
    assert_eq!(uri.scheme(), scheme);
    assert_eq!(uri.host_str(), Some("127.0.0.1"));
    assert!(uri.port().is_some());
    assert!(uri.username().is_empty() && uri.password().is_none());
    assert!(uri.query().is_none() && uri.fragment().is_none());
    assert_eq!(uri.path(), path);
}

#[tokio::test]
#[ignore = "Explicit ephemeral HTTPS Azurite only; never uses Azure resources or personal credentials."]
async fn https_azure_contract() {
    assert_eq!(std::env::consts::OS, "linux");
    let root = PathBuf::from(std::env::var_os("VOICEPROMPT_AZURITE_CONTRACT_ROOT").unwrap());
    assert!(root.is_absolute() && root.is_dir());
    assert!(!std::fs::symlink_metadata(&root).unwrap().is_symlink());
    let setup_path = root.join("fixture.json");
    assert!(std::fs::metadata(&setup_path).unwrap().len() <= 16_384);
    let setup: Setup = serde_json::from_slice(&std::fs::read(setup_path).unwrap()).unwrap();
    assert_eq!(setup.scope, "https-azurite-production-rest-contract");
    let bases =
        [setup.blob, setup.queue, setup.table].map(|value| value.parse::<url::Url>().unwrap());
    for uri in &bases {
        endpoint(uri, "https", "/devstoreaccount1/");
    }
    let [blob, queue, table] = bases;
    let identity: url::Url = std::env::var("IDENTITY_ENDPOINT").unwrap().parse().unwrap();
    endpoint(&identity, "http", "/token");
    assert_eq!(std::env::var("AZURE_CLIENT_ID").unwrap(), "synthetic-uami");
    let certificate = std::fs::read(root.join("server-cert.pem")).unwrap();
    assert!(certificate.len() <= 16_384);
    let storage = AzureStorage {
        credential: Arc::new(ManagedIdentity::from_environment().unwrap()),
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .add_root_certificate(reqwest::Certificate::from_pem(&certificate).unwrap())
            .build()
            .unwrap(),
        blob,
        queue,
        table,
    };
    let config = Config::parse(
        &[
            ("VR_ENVIRONMENT", "test"),
            ("VR_GOOGLE_ALLOWED_AUDIENCES", "synthetic"),
            ("VR_ALLOWLISTED_EMAIL", "synthetic@example.invalid"),
            ("VR_STORAGE_ACCOUNT_NAME", "devstoreaccount1"),
            ("VR_FOUNDRY_ENDPOINT", "https://fixture.invalid"),
            ("VR_SPEECH_ENDPOINT", "https://fixture.invalid"),
            ("VR_WEBPUBSUB_ENDPOINT", "https://fixture.invalid"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect(),
    )
    .unwrap();
    storage
        .check_resources(&config)
        .await
        .expect("Read-only real emulator resource probes");

    let seed = storage
        .entity("recordings", "synthetic", "python-seed")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seed.values["counter"], "1099511627776");
    assert_eq!(seed.values["counter@odata.type"], "Edm.Int64");
    assert_eq!(seed.values["small"], 7);
    let native = json!({
        "PartitionKey":"synthetic", "RowKey":"native'quote", "text":"synthetic \u{159} text",
        "counter":"1099511627776", "counter@odata.type":"Edm.Int64", "small":7
    });
    let etag = storage
        .insert("recordings", native.as_object().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        storage
            .insert("recordings", native.as_object().unwrap())
            .await,
        Err(StorageError::Conflict)
    ));
    let mut entity = storage
        .entity("recordings", "synthetic", "native'quote")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entity.etag, etag);
    let previous = entity.clone();
    entity.values.insert("small".into(), 8.into());
    entity.etag = storage.replace("recordings", &entity).await.unwrap();
    assert_ne!(entity.etag, previous.etag);
    assert!(matches!(
        storage.replace("recordings", &previous).await,
        Err(StorageError::Conflict)
    ));
    assert!(matches!(
        storage
            .delete_entity("recordings", "synthetic", "native'quote", &previous.etag)
            .await,
        Err(StorageError::Conflict)
    ));
    let rows = storage
        .query_partition("recordings", "synthetic")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.etag.is_empty()));
    assert!(
        rows.iter()
            .all(|row| row.values["counter@odata.type"] == "Edm.Int64")
    );

    storage
        .put_blob_if_absent(
            "audio",
            "nested/native-lease",
            b"first".to_vec(),
            "application/octet-stream",
        )
        .await
        .unwrap();
    assert!(matches!(
        storage
            .put_blob_if_absent(
                "audio",
                "nested/native-lease",
                b"wrong".to_vec(),
                "application/octet-stream"
            )
            .await,
        Err(StorageError::Conflict)
    ));
    let first = crate::services::uuid().unwrap();
    let second = crate::services::uuid().unwrap();
    assert_eq!(
        storage
            .acquire_blob_lease("audio", "nested/native-lease", &first)
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        storage
            .acquire_blob_lease("audio", "nested/native-lease", &second)
            .await,
        Err(StorageError::Conflict)
    ));
    storage
        .renew_blob_lease("audio", "nested/native-lease", &first)
        .await
        .unwrap();
    assert!(matches!(
        storage
            .put_leased_blob(
                "audio",
                "nested/native-lease",
                b"wrong".to_vec(),
                "application/octet-stream",
                &second
            )
            .await,
        Err(StorageError::Conflict)
    ));
    storage
        .put_leased_blob(
            "audio",
            "nested/native-lease",
            b"updated".to_vec(),
            "application/octet-stream",
            &first,
        )
        .await
        .unwrap();
    assert_eq!(
        storage
            .get_blob("audio", "nested/native-lease", 1024)
            .await
            .unwrap(),
        b"updated"
    );
    assert_eq!(
        storage.list_blobs("audio", "nested/").await.unwrap().len(),
        1
    );
    storage
        .release_blob_lease("audio", "nested/native-lease", &first)
        .await
        .unwrap();
    storage
        .acquire_blob_lease("audio", "nested/native-lease", &second)
        .await
        .unwrap();
    assert!(matches!(
        storage
            .release_blob_lease("audio", "nested/native-lease", &first)
            .await,
        Err(StorageError::Conflict)
    ));
    assert!(matches!(
        storage
            .delete_leased_blob("audio", "nested/native-lease", &first)
            .await,
        Err(StorageError::Conflict)
    ));
    assert!(
        storage
            .delete_leased_blob("audio", "nested/native-lease", &second)
            .await
            .unwrap()
    );
    assert!(
        !storage
            .delete_blob("audio", "nested/native-lease")
            .await
            .unwrap()
    );

    let seed = storage.receive_queue("work", 30).await.unwrap();
    assert_eq!(seed.len(), 1);
    assert_eq!(
        seed[0].content,
        json!({"direction":"python-to-rust","text":"synthetic \u{159}"})
    );
    storage.delete_queue("work", &seed[0]).await.unwrap();
    let payload = json!({"direction":"native-renewal","text":"synthetic \u{159}"});
    storage.send_queue("work", &payload, 0).await.unwrap();
    let original = storage.receive_queue("work", 30).await.unwrap().remove(0);
    assert_eq!(original.content, payload);
    let renewed = storage.renew_queue("work", &original, 30).await.unwrap();
    assert_ne!(renewed.pop_receipt, original.pop_receipt);
    // A stale delete may be rejected or idempotently acknowledged, but must not remove the winner.
    let _stale = storage.delete_queue("work", &original).await;
    let visible = storage.renew_queue("work", &renewed, 0).await.unwrap();
    let redelivered = storage.receive_queue("work", 30).await.unwrap().remove(0);
    assert_eq!(redelivered.id, original.id);
    assert_eq!(redelivered.content, payload);
    assert_eq!(redelivered.dequeue_count, original.dequeue_count + 1);
    assert!(storage.renew_queue("work", &visible, 30).await.is_err());
    storage.delete_queue("work", &redelivered).await.unwrap();
    assert!(storage.receive_queue("work", 30).await.unwrap().is_empty());
    storage
        .send_queue(
            "interop",
            &json!({"direction":"rust-to-python","text":"synthetic \u{159}"}),
            0,
        )
        .await
        .unwrap();
    storage
        .put_blob_if_absent(
            "raw",
            "finalize/zero-length.lock",
            Vec::new(),
            "text/plain; charset=utf-8",
        )
        .await
        .unwrap();
    assert!(
        storage
            .get_blob("raw", "finalize/zero-length.lock", 32)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        storage
            .delete_blob("raw", "finalize/zero-length.lock")
            .await
            .unwrap()
    );
    let storage = Arc::new(storage);
    let (legacy_recording_id, legacy_transcript_id) = worker_and_cleanup(storage.clone()).await;
    let (continuous_recording_id, continuous_transcript_id) = continuous_worker(storage).await;
    println!(
        "{}",
        json!({
            "scope":"https-azurite-production-rest-contract", "tls_verified":true,
            "table_cas_and_int64":true, "blob_lease_fencing":true,
            "queue_receipt_rotation_and_redelivery":true,
            "legacy_queue_worker_and_durable_delivery":true,
            "expired_cleanup_respects_active_blob_lease":true,
            "legacy_recording_id":legacy_recording_id,
            "legacy_transcript_id":legacy_transcript_id,
            "continuous_single_stream_and_exact_samples":true,
            "audio_retained_until_durable_finalization":true,
            "continuous_recording_id":continuous_recording_id,
            "continuous_transcript_id":continuous_transcript_id,
            "real_azure_resources":false, "real_managed_identity":false,
            "production_authorization_proven":false,
        })
    );
}
