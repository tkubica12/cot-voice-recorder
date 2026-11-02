use super::*;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

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
    println!(
        "{}",
        json!({
            "scope":"https-azurite-production-rest-contract", "tls_verified":true,
            "table_cas_and_int64":true, "blob_lease_fencing":true,
            "queue_receipt_rotation_and_redelivery":true,
            "real_azure_resources":false, "real_managed_identity":false,
            "production_authorization_proven":false,
        })
    );
}
