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

impl AzureStorage {
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
            request = request.body(body);
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
    pub async fn delete_blob(&self, container: &str, name: &str) -> Result<bool> {
        let uri = Self::uri(&self.blob, &[container, name])?;
        match self.request(Method::DELETE, uri, None, &[]).await {
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
                &[("Accept", "application/json;odata=nometadata")],
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
        let values = serde_json::from_slice(&Self::bytes(response, 4 * 1024 * 1024).await?)
            .map_err(|_| error())?;
        Ok(Some(Entity { values, etag }))
    }
    pub async fn insert(
        &self,
        table: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        let uri = Self::uri(&self.table, &[table])?;
        let body = encode_entity(values)?;
        self.request(
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
        Ok(())
    }
    pub async fn replace(&self, table: &str, entity: &Entity) -> Result<()> {
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
        self.request(
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
        Ok(())
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
            let body = r#"{"PartitionKey":"recording","RowKey":"fixture","state":"recording"}"#;
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
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["state"],
                "transcribing"
            );
            stream.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let storage = fixture(port, identity);
        let mut entity = storage
            .entity("recordings", "recording", "fixture")
            .await
            .unwrap()
            .unwrap();
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
}
