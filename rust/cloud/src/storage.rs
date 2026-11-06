use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use reqwest::{Method, Response};
use serde::Deserialize;

use crate::config::Config;
use crate::identity::ManagedIdentity;
use crate::problem::Problem;

pub struct AzureStorage {
    credential: Arc<ManagedIdentity>,
    http: reqwest::Client,
    blob: url::Url,
    queue: url::Url,
    table: url::Url,
}

#[derive(Clone)]
pub struct Entity {
    pub values: serde_json::Map<String, serde_json::Value>,
    pub etag: String,
}
#[derive(Clone)]
pub struct BlobEntry {
    pub name: String,
    pub modified_at: time::OffsetDateTime,
}

#[derive(Clone)]
pub struct QueueMessage {
    pub id: String,
    pub pop_receipt: String,
    pub dequeue_count: u32,
    pub content: serde_json::Value,
}

#[derive(Debug)]
pub enum StorageError {
    NotFound,
    Conflict,
    MessageGone,
    Failure(Problem),
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Storage operation: {self:?}")
    }
}
impl std::error::Error for StorageError {}
type Result<T> = std::result::Result<T, StorageError>;

fn error() -> StorageError {
    StorageError::Failure(Problem::unavailable("Azure storage operation failed."))
}
fn invalid() -> StorageError {
    StorageError::Failure(Problem::validation(
        "Invalid storage key or conditional request.",
    ))
}

#[cfg(test)]
mod loopback_fixture_tests {
    use super::*;

    #[test]
    fn fixture_rejects_nonlocal_unverified_or_ambiguous_endpoints_before_any_request() {
        let credential = Arc::new(ManagedIdentity::fixture("http://127.0.0.1:12345/token"));
        let valid = url::Url::parse("https://127.0.0.1:12345/devstoreaccount1/").unwrap();
        for endpoint in [
            "http://127.0.0.1:12345/devstoreaccount1/",
            "https://example.invalid:12345/devstoreaccount1/",
            "https://user@127.0.0.1:12345/devstoreaccount1/",
            "https://127.0.0.1:12345/devstoreaccount1/?query=value",
            "https://127.0.0.1:12345/devstoreaccount1/#fragment",
            "https://127.0.0.1:12345/another-account/",
        ] {
            let error = AzureStorage::loopback_fixture(
                credential.clone(),
                [
                    url::Url::parse(endpoint).unwrap(),
                    valid.clone(),
                    valid.clone(),
                ],
                b"not a certificate",
            )
            .err()
            .unwrap();
            assert_eq!(
                error.detail,
                "Storage fixture requires explicit verified-HTTPS loopback Azurite endpoints."
            );
        }
        let error = AzureStorage::loopback_fixture(
            credential,
            [valid.clone(), valid.clone(), valid],
            b"not a certificate",
        )
        .err()
        .unwrap();
        assert_eq!(
            error.detail,
            "Invalid explicit storage fixture certificate."
        );
    }
}

impl AzureStorage {
    #[cfg(any(test, feature = "loopback-fixtures"))]
    pub fn loopback_fixture(
        credential: Arc<ManagedIdentity>,
        endpoints: [url::Url; 3],
        certificate_pem: &[u8],
    ) -> std::result::Result<Self, Problem> {
        for endpoint in &endpoints {
            if endpoint.scheme() != "https"
                || endpoint.host_str() != Some("127.0.0.1")
                || endpoint.port().is_none()
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || endpoint.query().is_some()
                || endpoint.fragment().is_some()
                || endpoint.path() != "/devstoreaccount1/"
            {
                return Err(Problem::validation(
                    "Storage fixture requires explicit verified-HTTPS loopback Azurite endpoints.",
                ));
            }
        }
        let certificate = reqwest::Certificate::from_pem(certificate_pem)
            .map_err(|_| Problem::validation("Invalid explicit storage fixture certificate."))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(certificate)
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        let [blob, queue, table] = endpoints;
        Ok(Self {
            credential,
            http,
            blob,
            queue,
            table,
        })
    }

    pub async fn check_resources(&self, config: &Config) -> Result<()> {
        for container in [
            &config.audio_container,
            &config.transcript_container,
            &config.raw_container,
        ] {
            let mut uri = Self::uri(&self.blob, &[container])?;
            uri.query_pairs_mut().append_pair("restype", "container");
            self.request(Method::HEAD, uri, None, &[]).await?;
        }

        let mut uri = Self::uri(&self.queue, &[&config.queue])?;
        uri.query_pairs_mut().append_pair("comp", "metadata");
        self.request(Method::GET, uri, None, &[]).await?;
        for table in [
            &config.recordings_table,
            &config.chunks_table,
            &config.transcripts_table,
        ] {
            let uri = Self::uri(&self.table, &[&format!("Tables('{}')", odata_key(table)?)])?;
            self.request(
                Method::GET,
                uri,
                None,
                &[("Accept", "application/json;odata=nometadata")],
            )
            .await?;
        }
        Ok(())
    }
    pub fn new(
        config: &Config,
        credential: Arc<ManagedIdentity>,
    ) -> std::result::Result<Self, Problem> {
        let base = |kind| {
            url::Url::parse(&format!(
                "https://{}.{}.core.windows.net/",
                config.storage_account, kind
            ))
            .map_err(|_| Problem::validation("Invalid configured storage endpoint."))
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        Ok(Self {
            credential,
            http,
            blob: base("blob")?,
            queue: base("queue")?,
            table: base("table")?,
        })
    }
    fn uri(base: &url::Url, segments: &[&str]) -> Result<url::Url> {
        let mut uri = base.clone();
        let mut path = uri.path_segments_mut().map_err(|_| invalid())?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(uri)
    }
    async fn request(
        &self,
        method: Method,
        uri: url::Url,
        body: Option<Vec<u8>>,
        headers: &[(&str, &str)],
    ) -> Result<Response> {
        let token = self
            .credential
            .token("https://storage.azure.com/")
            .await
            .map_err(StorageError::Failure)?;
        let date = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc2822)
            .map_err(|_| error())?
            .replace("+0000", "GMT");
        // Azure Storage answers 411 to a PUT/POST (lease, queue update) that omits Content-Length.
        let body = match body {
            None if matches!(method, Method::PUT | Method::POST) => Some(Vec::new()),
            body => body,
        };
        let mut request = self
            .http
            .request(method, uri)
            .bearer_auth(token)
            .header("x-ms-version", "2023-11-03")
            .header("x-ms-date", date);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.header("Content-Length", body.len()).body(body);
        }
        let response = request.send().await.map_err(|_| error())?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            404 => Err(StorageError::NotFound),
            409 | 412 => Err(StorageError::Conflict),
            400 if response
                .headers()
                .get("x-ms-error-code")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    matches!(value, "PopReceiptMismatch" | "InvalidQueryParameterValue")
                }) =>
            {
                Err(StorageError::MessageGone)
            }
            status => {
                eprintln!("Azure storage returned status {status}; response content is redacted.");
                Err(error())
            }
        }
    }
    async fn bytes(mut response: Response, maximum: usize) -> Result<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return Err(error());
        }
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| error())? {
            if chunk.len() > maximum - data.len() {
                return Err(error());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    pub async fn put_blob(
        &self,
        container: &str,
        name: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        let uri = Self::uri(&self.blob, &[container, name])?;
        self.request(
            Method::PUT,
            uri,
            Some(data),
            &[
                ("x-ms-blob-type", "BlockBlob"),
                ("Content-Type", content_type),
            ],
        )
        .await?;
        Ok(())
    }
    pub async fn get_blob(&self, container: &str, name: &str, maximum: usize) -> Result<Vec<u8>> {
        let uri = Self::uri(&self.blob, &[container, name])?;
        Self::bytes(self.request(Method::GET, uri, None, &[]).await?, maximum).await
    }
    pub async fn put_blob_if_absent(
        &self,
        container: &str,
        name: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        self.request(
            Method::PUT,
            Self::uri(&self.blob, &[container, name])?,
            Some(data),
            &[
                ("x-ms-blob-type", "BlockBlob"),
                ("Content-Type", content_type),
                ("If-None-Match", "*"),
            ],
        )
        .await?;
        Ok(())
    }
    pub async fn list_blobs(&self, container: &str, prefix: &str) -> Result<Vec<BlobEntry>> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(rename = "Blobs", default)]
            blobs: Items,
            #[serde(rename = "NextMarker", default)]
            next: String,
        }
        #[derive(Deserialize, Default)]
        struct Items {
            #[serde(rename = "Blob", default)]
            blobs: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            #[serde(rename = "Name")]
            name: String,
            #[serde(rename = "Properties")]
            properties: Properties,
        }
        #[derive(Deserialize)]
        struct Properties {
            #[serde(rename = "Last-Modified")]
            modified: String,
        }
        let mut marker = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for _ in 0..10000 {
            let mut uri = Self::uri(&self.blob, &[container])?;
            uri.query_pairs_mut()
                .append_pair("restype", "container")
                .append_pair("comp", "list")
                .append_pair("prefix", prefix)
                .append_pair("maxresults", "5000");
            if !marker.is_empty() {
                uri.query_pairs_mut().append_pair("marker", &marker);
            }
            let response = self.request(Method::GET, uri, None, &[]).await?;
            let bytes = Self::bytes(response, 16 * 1024 * 1024).await?;
            let listing: Listing =
                quick_xml::de::from_reader(bytes.as_slice()).map_err(|_| error())?;
            for item in listing.blobs.blobs {
                if item.name.is_empty() || !item.name.starts_with(prefix) {
                    return Err(error());
                }
                let modified_at = time::OffsetDateTime::parse(
                    &item.properties.modified,
                    &time::format_description::well_known::Rfc2822,
                )
                .map_err(|_| error())?;
                result.push(BlobEntry {
                    name: item.name,
                    modified_at,
                });
                if result.len() > 1_000_000 {
                    return Err(error());
                }
            }
            if listing.next.is_empty() {
                return Ok(result);
            }
            if listing.next.len() > 64 * 1024 || !seen.insert(listing.next.clone()) {
                return Err(error());
            }
            marker = listing.next;
        }
        Err(error())
    }
    pub async fn put_leased_blob(
        &self,
        container: &str,
        name: &str,
        data: Vec<u8>,
        content_type: &str,
        lease: &str,
    ) -> Result<()> {
        if !valid_lease(lease) {
            return Err(invalid());
        }
        self.request(
            Method::PUT,
            Self::uri(&self.blob, &[container, name])?,
            Some(data),
            &[
                ("x-ms-blob-type", "BlockBlob"),
                ("Content-Type", content_type),
                ("x-ms-lease-id", lease),
            ],
        )
        .await?;
        Ok(())
    }
    pub async fn acquire_blob_lease(
        &self,
        container: &str,
        name: &str,
        proposed: &str,
    ) -> Result<String> {
        if !valid_lease(proposed) {
            return Err(invalid());
        }
        let mut uri = Self::uri(&self.blob, &[container, name])?;
        uri.query_pairs_mut().append_pair("comp", "lease");
        let response = self
            .request(
                Method::PUT,
                uri,
                None,
                &[
                    ("x-ms-lease-action", "acquire"),
                    ("x-ms-lease-duration", "60"),
                    ("x-ms-proposed-lease-id", proposed),
                ],
            )
            .await?;
        response
            .headers()
            .get("x-ms-lease-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_lease(value))
            .map(str::to_owned)
            .ok_or_else(error)
    }
    pub async fn renew_blob_lease(&self, container: &str, name: &str, lease: &str) -> Result<()> {
        self.blob_lease_action(container, name, lease, "renew")
            .await
    }
    pub async fn release_blob_lease(&self, container: &str, name: &str, lease: &str) -> Result<()> {
        self.blob_lease_action(container, name, lease, "release")
            .await
    }
    async fn blob_lease_action(
        &self,
        container: &str,
        name: &str,
        lease: &str,
        action: &'static str,
    ) -> Result<()> {
        if !valid_lease(lease) {
            return Err(invalid());
        }
        let mut uri = Self::uri(&self.blob, &[container, name])?;
        uri.query_pairs_mut().append_pair("comp", "lease");
        self.request(
            Method::PUT,
            uri,
            None,
            &[("x-ms-lease-action", action), ("x-ms-lease-id", lease)],
        )
        .await?;
        Ok(())
    }
    pub async fn delete_blob(&self, container: &str, name: &str) -> Result<bool> {
        let uri = Self::uri(&self.blob, &[container, name])?;
        match self.request(Method::DELETE, uri, None, &[]).await {
            Ok(_) => Ok(true),
            Err(StorageError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub async fn delete_leased_blob(
        &self,
        container: &str,
        name: &str,
        lease: &str,
    ) -> Result<bool> {
        if !valid_lease(lease) {
            return Err(invalid());
        }
        match self
            .request(
                Method::DELETE,
                Self::uri(&self.blob, &[container, name])?,
                None,
                &[("x-ms-lease-id", lease)],
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(StorageError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn entity(&self, table: &str, partition: &str, row: &str) -> Result<Option<Entity>> {
        let uri = entity_uri(&self.table, table, partition, row)?;
        let response = match self
            .request(
                Method::GET,
                uri,
                None,
                &[("Accept", "application/json;odata=fullmetadata")],
            )
            .await
        {
            Ok(response) => response,
            Err(StorageError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let etag = response
            .headers()
            .get("ETag")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(error)?
            .to_owned();
        let mut values: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&Self::bytes(response, 4 * 1024 * 1024).await?)
                .map_err(|_| error())?;
        values.remove("odata.etag");
        Ok(Some(Entity { values, etag }))
    }
    pub async fn insert(
        &self,
        table: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String> {
        let uri = Self::uri(&self.table, &[table])?;
        let body = encode_entity(values)?;
        let response = self
            .request(
                Method::POST,
                uri,
                Some(body),
                &[
                    ("Content-Type", "application/json"),
                    ("Accept", "application/json;odata=nometadata"),
                    ("Prefer", "return-no-content"),
                ],
            )
            .await?;
        response_etag(&response)
    }
    pub async fn replace(&self, table: &str, entity: &Entity) -> Result<String> {
        let partition = entity
            .values
            .get("PartitionKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let row = entity
            .values
            .get("RowKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        if entity.etag.is_empty() || entity.etag == "*" {
            return Err(invalid());
        }
        let response = self
            .request(
                Method::PUT,
                entity_uri(&self.table, table, partition, row)?,
                Some(encode_entity(&entity.values)?),
                &[
                    ("Content-Type", "application/json"),
                    ("Accept", "application/json;odata=nometadata"),
                    ("If-Match", &entity.etag),
                ],
            )
            .await?;
        response_etag(&response)
    }
    pub async fn delete_entity(
        &self,
        table: &str,
        partition: &str,
        row: &str,
        etag: &str,
    ) -> Result<()> {
        if etag.is_empty() || etag == "*" {
            return Err(invalid());
        }
        self.request(
            Method::DELETE,
            entity_uri(&self.table, table, partition, row)?,
            None,
            &[("If-Match", etag)],
        )
        .await?;
        Ok(())
    }
    pub async fn query_partition(&self, table: &str, partition: &str) -> Result<Vec<Entity>> {
        let mut uri = Self::uri(&self.table, &[table])?;
        uri.query_pairs_mut().append_pair(
            "$filter",
            &format!("PartitionKey eq '{}'", odata_key(partition)?),
        );
        let mut all = Vec::new();
        let mut cursors = std::collections::HashSet::new();
        loop {
            let response = self
                .request(
                    Method::GET,
                    uri.clone(),
                    None,
                    &[("Accept", "application/json;odata=fullmetadata")],
                )
                .await?;
            let next_pk = response
                .headers()
                .get("x-ms-continuation-NextPartitionKey")
                .map(|value| value.to_str().map(str::to_owned).map_err(|_| error()))
                .transpose()?;
            let next_rk = response
                .headers()
                .get("x-ms-continuation-NextRowKey")
                .map(|value| value.to_str().map(str::to_owned).map_err(|_| error()))
                .transpose()?;
            #[derive(Deserialize)]
            struct Page {
                value: Vec<serde_json::Map<String, serde_json::Value>>,
            }
            let page: Page =
                serde_json::from_slice(&Self::bytes(response, 16 * 1024 * 1024).await?)
                    .map_err(|_| error())?;
            for mut values in page.value {
                let etag = values
                    .remove("odata.etag")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .ok_or_else(error)?;
                all.push(Entity { values, etag });
                if all.len() > 1_000_000 {
                    return Err(error());
                }
            }
            if next_pk.is_none() && next_rk.is_none() {
                break;
            }
            if !cursors.insert((next_pk.clone(), next_rk.clone())) || cursors.len() > 10_000 {
                eprintln!("Azure Table continuation did not advance within bounded pagination.");
                return Err(error());
            }
            let mut next = Self::uri(&self.table, &[table])?;
            next.query_pairs_mut().append_pair(
                "$filter",
                &format!("PartitionKey eq '{}'", odata_key(partition)?),
            );
            if let Some(pk) = next_pk {
                next.query_pairs_mut().append_pair("NextPartitionKey", &pk);
            }
            if let Some(rk) = next_rk {
                next.query_pairs_mut().append_pair("NextRowKey", &rk);
            }
            uri = next;
        }
        Ok(all)
    }

    pub async fn send_queue(
        &self,
        queue: &str,
        content: &serde_json::Value,
        delay: u32,
    ) -> Result<()> {
        if delay > 7 * 24 * 60 * 60 {
            return Err(invalid());
        }
        let mut uri = Self::uri(&self.queue, &[queue, "messages"])?;
        uri.query_pairs_mut()
            .append_pair("visibilitytimeout", &delay.to_string());
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(content).map_err(|_| invalid())?);
        let body = format!("<QueueMessage><MessageText>{encoded}</MessageText></QueueMessage>");
        self.request(
            Method::POST,
            uri,
            Some(body.into_bytes()),
            &[("Content-Type", "application/xml")],
        )
        .await?;
        Ok(())
    }
    pub async fn receive_queue(&self, queue: &str, visibility: u32) -> Result<Vec<QueueMessage>> {
        let mut uri = Self::uri(&self.queue, &[queue, "messages"])?;
        uri.query_pairs_mut()
            .append_pair("numofmessages", "1")
            .append_pair("visibilitytimeout", &visibility.to_string());
        let data = Self::bytes(
            self.request(Method::GET, uri, None, &[]).await?,
            1024 * 1024,
        )
        .await?;
        decode_queue(&data)
    }
    pub async fn renew_queue(
        &self,
        queue: &str,
        message: &QueueMessage,
        visibility: u32,
    ) -> Result<QueueMessage> {
        let mut uri = Self::uri(&self.queue, &[queue, "messages", &message.id])?;
        uri.query_pairs_mut()
            .append_pair("popreceipt", &message.pop_receipt)
            .append_pair("visibilitytimeout", &visibility.to_string());
        let response = match self.request(Method::PUT, uri, None, &[]).await {
            Err(StorageError::NotFound) => return Err(StorageError::MessageGone),
            result => result?,
        };
        let receipt = response
            .headers()
            .get("x-ms-popreceipt")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(error)?;
        Ok(QueueMessage {
            pop_receipt: receipt.into(),
            ..message.clone()
        })
    }
    pub async fn delete_queue(&self, queue: &str, message: &QueueMessage) -> Result<()> {
        let mut uri = Self::uri(&self.queue, &[queue, "messages", &message.id])?;
        uri.query_pairs_mut()
            .append_pair("popreceipt", &message.pop_receipt);
        match self.request(Method::DELETE, uri, None, &[]).await {
            Ok(_) => Ok(()),
            Err(StorageError::NotFound | StorageError::MessageGone) => {
                eprintln!(
                    "Queue delete receipt was already gone; idempotent deletion acknowledged."
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn odata_key(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 1024
        || value
            .chars()
            .any(|value| value.is_control() || matches!(value, '/' | '\\' | '#' | '?'))
    {
        return Err(invalid());
    }
    Ok(value.replace('\'', "''"))
}
pub(crate) fn valid_lease(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, value)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                value == b'-'
            } else {
                value.is_ascii_hexdigit()
            }
        })
}
fn response_etag(response: &Response) -> Result<String> {
    response
        .headers()
        .get("ETag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && *value != "*")
        .map(str::to_owned)
        .ok_or_else(|| {
            StorageError::Failure(Problem::unavailable(
                "Table write returned no usable version ETag.",
            ))
        })
}
fn entity_uri(base: &url::Url, table: &str, partition: &str, row: &str) -> Result<url::Url> {
    AzureStorage::uri(
        base,
        &[&format!(
            "{table}(PartitionKey='{}',RowKey='{}')",
            odata_key(partition)?,
            odata_key(row)?
        )],
    )
}
fn encode_entity(values: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<u8>> {
    if !values.contains_key("PartitionKey") || !values.contains_key("RowKey") {
        return Err(invalid());
    }
    let values: serde_json::Map<_, _> = values
        .iter()
        .filter(|(key, value)| !value.is_null() && !key.starts_with("odata."))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    serde_json::to_vec(&values).map_err(|_| invalid())
}

fn decode_queue(data: &[u8]) -> Result<Vec<QueueMessage>> {
    #[derive(Deserialize)]
    struct Messages {
        #[serde(rename = "QueueMessage", default)]
        messages: Vec<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        #[serde(rename = "MessageId")]
        id: String,
        #[serde(rename = "PopReceipt")]
        receipt: String,
        #[serde(rename = "DequeueCount")]
        count: u32,
        #[serde(rename = "MessageText")]
        text: String,
    }
    let messages: Messages = quick_xml::de::from_reader(data).map_err(|_| error())?;
    messages
        .messages
        .into_iter()
        .map(|message| {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(message.text)
                .map_err(|_| error())?;
            Ok(QueueMessage {
                id: message.id,
                pop_receipt: message.receipt,
                dequeue_count: message.count,
                content: serde_json::from_slice(&decoded).map_err(|_| error())?,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "storage_azurite_tests.rs"]
mod azurite_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_keys_null_fields_and_base64_queue_messages_preserve_azure_contracts() {
        let base = url::Url::parse("https://fixture.table.core.windows.net/").unwrap();
        let uri = entity_uri(&base, "recordings", "crid", "synthetic'quote").unwrap();
        assert!(uri.as_str().contains("synthetic''quote"));
        assert!(entity_uri(&base, "recordings", "recording", "../escape").is_err());
        let values = serde_json::json!({"PartitionKey":"recording","RowKey":"synthetic","state":"recording","missing":null,"odata.etag":"etag"});
        let encoded: serde_json::Value =
            serde_json::from_slice(&encode_entity(values.as_object().unwrap()).unwrap()).unwrap();
        assert!(encoded.get("missing").is_none());
        assert!(encoded.get("odata.etag").is_none());
        let content = serde_json::json!({"type":"stream","recording_id":"synthetic"});
        let text = base64::engine::general_purpose::STANDARD.encode(content.to_string());
        let xml = format!(
            "<QueueMessagesList><QueueMessage><MessageId>id</MessageId><PopReceipt>receipt</PopReceipt><DequeueCount>2</DequeueCount><MessageText>{text}</MessageText></QueueMessage></QueueMessagesList>"
        );
        let messages = decode_queue(xml.as_bytes()).unwrap();
        assert_eq!(messages[0].content, content);
        assert_eq!(messages[0].dequeue_count, 2);
        assert!(decode_queue(b"<invalid>").is_err());
    }

    async fn identity_fixture() -> (Arc<ManagedIdentity>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!(
            "http://127.0.0.1:{}/token",
            listener.local_addr().unwrap().port()
        );
        let response = serde_json::json!({"access_token":"synthetic-storage-token","expires_on":time::OffsetDateTime::now_utc().unix_timestamp()+3600}).to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = [0; 8192];
            assert!(stream.read(&mut data).await.unwrap() > 0);
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        });
        (Arc::new(ManagedIdentity::fixture(&endpoint)), task)
    }
    fn fixture(port: u16, identity: Arc<ManagedIdentity>) -> AzureStorage {
        let base = url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        AzureStorage {
            credential: identity,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            blob: base.clone(),
            queue: base.clone(),
            table: base,
        }
    }
    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let mut data = Vec::new();
        let mut buffer = [0; 4096];
        let end = loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            data.extend_from_slice(&buffer[..count]);
            if let Some(end) = data.windows(4).position(|value| value == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8(data[..end].to_vec()).unwrap();
        let count = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = data[end..].to_vec();
        while body.len() < count {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            body.extend_from_slice(&buffer[..count]);
        }
        (headers, body)
    }

    #[tokio::test]
    async fn real_table_request_preserves_etag_and_conflict_is_not_an_unconditional_retry() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, _) = read_request(&mut stream).await;
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-storage-token")
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("accept: application/json;odata=fullmetadata")
            );
            let body = r#"{"PartitionKey":"recording","RowKey":"fixture","state":"recording","large":"1099511627776","large@odata.type":"Edm.Int64","created":"2026-11-02T00:00:00Z","created@odata.type":"Edm.DateTime","odata.etag":"body-metadata-is-not-the-write-version"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nETag: W/\"synthetic-etag\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            assert!(headers.starts_with("PUT "));
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("if-match: w/\"synthetic-etag\"")
            );
            let encoded: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(encoded["state"], "transcribing");
            assert_eq!(encoded["large"], "1099511627776");
            assert_eq!(encoded["large@odata.type"], "Edm.Int64");
            assert_eq!(encoded["created@odata.type"], "Edm.DateTime");
            assert!(encoded.get("odata.etag").is_none());
            stream.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let storage = fixture(port, identity);
        let mut entity = storage
            .entity("recordings", "recording", "fixture")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entity.values["large@odata.type"], "Edm.Int64");
        assert!(entity.values.get("odata.etag").is_none());
        entity
            .values
            .insert("state".into(), serde_json::json!("transcribing"));
        assert!(matches!(
            storage.replace("recordings", &entity).await,
            Err(StorageError::Conflict)
        ));
        entity.etag = "*".into();
        assert!(matches!(
            storage.replace("recordings", &entity).await,
            Err(StorageError::Failure(_))
        ));
        server.await.unwrap();
        identity_server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_blob_lock_declares_zero_length_instead_of_implicit_or_chunked_body() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("content-length: 0\r\n")
            );
            assert!(!headers.to_ascii_lowercase().contains("transfer-encoding:"));
            assert!(body.is_empty());
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        fixture(port, identity)
            .put_blob_if_absent(
                "raw",
                "finalize/synthetic.lock",
                Vec::new(),
                "text/plain; charset=utf-8",
            )
            .await
            .unwrap();
        server.await.unwrap();
        identity_server.await.unwrap();
    }

    #[tokio::test]
    async fn bodyless_lease_and_queue_updates_declare_zero_length_instead_of_omitting_it() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = read_request(&mut stream).await;
                let headers = headers.to_ascii_lowercase();
                assert!(headers.starts_with("put "));
                assert!(
                    headers.contains("content-length: 0\r\n"),
                    "Azure Storage rejects a body-less PUT without Content-Length with 411."
                );
                assert!(!headers.contains("transfer-encoding:"));
                assert!(body.is_empty());
                stream.write_all(b"HTTP/1.1 200 OK\r\nx-ms-lease-id: 00000000-0000-4000-8000-000000000001\r\nx-ms-popreceipt: renewed-receipt\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            }
        });
        let storage = fixture(port, identity);
        let lease = storage
            .acquire_blob_lease(
                "transcripts",
                "fixture.txt",
                "00000000-0000-4000-8000-000000000001",
            )
            .await
            .unwrap();
        storage
            .renew_blob_lease("transcripts", "fixture.txt", &lease)
            .await
            .unwrap();
        storage
            .release_blob_lease("transcripts", "fixture.txt", &lease)
            .await
            .unwrap();
        let message = QueueMessage {
            id: "message".into(),
            pop_receipt: "original-receipt".into(),
            dequeue_count: 1,
            content: serde_json::json!({"type":"stream","recording_id":"fixture"}),
        };
        let renewed = storage.renew_queue("work", &message, 300).await.unwrap();
        assert_eq!(renewed.pop_receipt, "renewed-receipt");
        server.await.unwrap();
        identity_server.await.unwrap();
    }

    #[tokio::test]
    async fn queue_renewal_replaces_receipt_and_stale_receipts_are_explicit() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for (status, extra) in [
                ("204 No Content", "x-ms-popreceipt: renewed-receipt\r\n"),
                ("400 Bad Request", "x-ms-error-code: PopReceiptMismatch\r\n"),
                ("404 Not Found", ""),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, _) = read_request(&mut stream).await;
                assert!(headers.contains("popreceipt="));
                stream.write_all(format!("HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let storage = fixture(port, identity);
        let original = QueueMessage {
            id: "message".into(),
            pop_receipt: "original+receipt".into(),
            dequeue_count: 2,
            content: serde_json::json!({"type":"stream","recording_id":"fixture"}),
        };
        let renewed = storage.renew_queue("work", &original, 300).await.unwrap();
        assert_eq!(renewed.pop_receipt, "renewed-receipt");
        assert_eq!(renewed.content, original.content);
        assert!(matches!(
            storage.renew_queue("work", &original, 300).await,
            Err(StorageError::MessageGone)
        ));
        storage.delete_queue("work", &renewed).await.unwrap();
        server.await.unwrap();
        identity_server.await.unwrap();
    }

    #[tokio::test]
    async fn repository_returns_its_own_write_version_without_a_racy_followup_get() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for (method, version) in [("POST", "writer-v1"), ("PUT", "writer-v2")] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = read_request(&mut stream).await;
                assert!(
                    headers.starts_with(method),
                    "Followup GET could return a different writer's entity."
                );
                let entity: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(entity["stream_lease_id"], "owner-a");
                if method == "PUT" {
                    assert!(headers.to_lowercase().contains("if-match: writer-v1"));
                }
                stream.write_all(format!("HTTP/1.1 204 No Content\r\nETag: {version}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let repository = crate::repository::Repository::Azure(Arc::new(fixture(port, identity)));
        let entity = Entity { values:serde_json::json!({"PartitionKey":"recording","RowKey":"fixture","stream_lease_id":"owner-a"}).as_object().unwrap().clone(), etag:String::new() };
        let first = repository.insert("recordings", entity).await.unwrap();
        assert_eq!(first.etag, "writer-v1");
        let second = repository.replace("recordings", first).await.unwrap();
        assert_eq!(second.etag, "writer-v2");
        assert_eq!(second.values["stream_lease_id"], "owner-a");
        server.await.unwrap();
        identity_server.await.unwrap();
    }
    #[tokio::test]
    async fn missing_or_wildcard_write_etags_fail_instead_of_adopting_a_different_snapshot() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for header in ["", "ETag: *\r\n"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_request(&mut stream).await;
                stream.write_all(format!("HTTP/1.1 204 No Content\r\n{header}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let storage = fixture(port, identity);
        let values = serde_json::json!({"PartitionKey":"recording","RowKey":"fixture"})
            .as_object()
            .unwrap()
            .clone();
        for _ in 0..2 {
            assert!(matches!(
                storage.insert("recordings", &values).await,
                Err(StorageError::Failure(_))
            ));
        }
        server.await.unwrap();
        identity_server.await.unwrap();
    }
    #[tokio::test]
    async fn canonical_blob_writes_use_a_server_lease_and_cannot_bypass_another_owner() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for (stage, status) in [(0, 201), (1, 200), (2, 201), (3, 412), (4, 200)] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = read_request(&mut stream).await;
                let headers = headers.to_ascii_lowercase();
                match stage {
                    0 => {
                        assert!(headers.contains("comp=lease"));
                        assert!(headers.contains("x-ms-lease-action: acquire"));
                        assert!(headers.contains("x-ms-lease-duration: 60"));
                        assert!(headers.contains(
                            "x-ms-proposed-lease-id: 00000000-0000-4000-8000-000000000001"
                        ));
                    }
                    1 => {
                        assert!(headers.contains("x-ms-lease-action: renew"));
                        assert!(
                            headers.contains("x-ms-lease-id: 00000000-0000-4000-8000-000000000001")
                        );
                    }
                    2 => {
                        assert!(
                            headers.contains("x-ms-lease-id: 00000000-0000-4000-8000-000000000001")
                        );
                        assert_eq!(body, b"synthetic canonical");
                    }
                    3 => assert!(
                        headers.contains("x-ms-lease-id: 00000000-0000-4000-8000-000000000002")
                    ),
                    4 => {
                        assert!(headers.contains("x-ms-lease-action: release"));
                        assert!(
                            headers.contains("x-ms-lease-id: 00000000-0000-4000-8000-000000000001")
                        );
                    }
                    _ => unreachable!(),
                }
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nx-ms-lease-id: 00000000-0000-4000-8000-000000000001\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let storage = fixture(port, identity);
        let lease = storage
            .acquire_blob_lease(
                "transcripts",
                "fixture.txt",
                "00000000-0000-4000-8000-000000000001",
            )
            .await
            .unwrap();
        assert_eq!(lease, "00000000-0000-4000-8000-000000000001");
        storage
            .renew_blob_lease("transcripts", "fixture.txt", &lease)
            .await
            .unwrap();
        storage
            .put_leased_blob(
                "transcripts",
                "fixture.txt",
                b"synthetic canonical".to_vec(),
                "text/plain",
                &lease,
            )
            .await
            .unwrap();
        assert!(matches!(
            storage
                .put_leased_blob(
                    "transcripts",
                    "fixture.txt",
                    b"bad".to_vec(),
                    "text/plain",
                    "00000000-0000-4000-8000-000000000002"
                )
                .await,
            Err(StorageError::Conflict)
        ));
        storage
            .release_blob_lease("transcripts", "fixture.txt", &lease)
            .await
            .unwrap();
        server.await.unwrap();
        identity_server.await.unwrap();
    }
    #[tokio::test]
    async fn blob_enumeration_is_complete_and_repeated_markers_cannot_be_a_truncated_success() {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for index in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, _) = read_request(&mut stream).await;
                assert!(headers.contains("comp=list") && headers.contains("prefix=fixture"));
                if index % 2 == 1 {
                    assert!(headers.contains("marker=next"));
                }
                let marker = if index == 1 { "" } else { "next" };
                let name = if index == 1 {
                    "fixture/second.wav"
                } else {
                    "fixture/first.wav"
                };
                let body = format!(
                    "<EnumerationResults><Blobs><Blob><Name>{name}</Name><Properties><Last-Modified>Sun, 01 Nov 2026 10:00:00 GMT</Last-Modified></Properties></Blob></Blobs><NextMarker>{marker}</NextMarker></EnumerationResults>"
                );
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let storage = fixture(port, identity);
        let entries = storage.list_blobs("audio", "fixture").await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "fixture/second.wav");
        assert_eq!(entries[0].modified_at.hour(), 10);
        assert!(storage.list_blobs("audio", "fixture").await.is_err());
        server.await.unwrap();
        identity_server.await.unwrap();
    }
    #[tokio::test]
    async fn bootstrap_probes_only_existing_resources_and_never_creates_them_or_reads_user_bodies()
    {
        use tokio::io::AsyncWriteExt;
        let (identity, identity_server) = identity_fixture().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for index in 0..7 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = read_request(&mut stream).await;
                assert!(body.is_empty());
                if index < 3 {
                    assert!(headers.starts_with("HEAD ") && headers.contains("restype=container"));
                } else if index == 3 {
                    assert!(headers.starts_with("GET ") && headers.contains("comp=metadata"));
                } else {
                    assert!(
                        headers.starts_with("GET ")
                            && headers.contains("Tables(")
                            && !headers.contains("%24top")
                    );
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let storage = fixture(port, identity);
        let config = crate::services::tests::fixture().config.clone();
        storage.check_resources(&config).await.unwrap();
        server.await.unwrap();
        identity_server.await.unwrap();
    }
}
