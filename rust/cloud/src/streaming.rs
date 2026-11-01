use crate::config::Config;
use crate::identity::ManagedIdentity;
use crate::problem::Problem;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct MaiStreamingProvider {
    endpoint: url::Url,
    deployment: String,
    identity: Arc<ManagedIdentity>,
}
pub struct Upstream {
    sender: UpstreamSender,
    stream: SplitStream<Socket>,
}
#[derive(Clone)]
pub struct UpstreamSender {
    sink: Arc<tokio::sync::Mutex<SplitSink<Socket, Message>>>,
}
fn interrupted() -> Problem {
    Problem::unavailable("MAI streaming interrupted; no partial text is confirmed.")
}
impl MaiStreamingProvider {
    pub fn new(
        config: &Config,
        identity: Arc<ManagedIdentity>,
        deployment: &str,
    ) -> Result<Self, Problem> {
        let mut endpoint = config.foundry.clone();
        if !matches!(endpoint.path(), "" | "/") {
            return Err(Problem::validation(
                "Foundry streaming requires a resource root.",
            ));
        }
        let local = config.environment != "production"
            && endpoint.scheme() == "http"
            && endpoint.host_str() == Some("127.0.0.1");
        if !local && endpoint.scheme() != "https" {
            return Err(Problem::validation("Foundry streaming requires HTTPS."));
        }
        let hostname = endpoint
            .host_str()
            .ok_or_else(|| Problem::validation("Missing Foundry streaming host."))?
            .replace(".cognitiveservices.azure.com", ".services.ai.azure.com");
        endpoint
            .set_host(Some(&hostname))
            .map_err(|_| Problem::validation("Invalid Foundry streaming host."))?;
        endpoint
            .set_scheme(if local { "ws" } else { "wss" })
            .map_err(|_| Problem::internal())?;
        endpoint.set_path("/mai/v1/realtime");
        endpoint.set_query(Some("intent=transcription"));
        Ok(Self {
            endpoint,
            deployment: deployment.into(),
            identity,
        })
    }
    pub async fn session(&self, language: &str) -> Result<Upstream, Problem> {
        let token = self
            .identity
            .token("https://cognitiveservices.azure.com/")
            .await?;
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|_| Problem::internal())?;
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| interrupted())?,
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES))
            .write_buffer_size(16 * 1024)
            .max_write_buffer_size(4 * 1024 * 1024);
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| interrupted())?
        .map_err(|_| interrupted())?;
        let (sink, stream) = socket.split();
        let mut upstream = Upstream {
            sender: UpstreamSender {
                sink: Arc::new(tokio::sync::Mutex::new(sink)),
            },
            stream,
        };
        upstream.wait_for("session.created").await?;
        let mut transcription = serde_json::json!({"model":self.deployment});
        if language != "auto" {
            transcription["language"] = Value::String(language.into());
        }
        upstream
            .send(serde_json::json!({"type":"session.update","session":{
                "type":"transcription","audio":{"input":{"format":{"type":"audio/pcm","rate":16000},
                "transcription":transcription,"turn_detection":null,"noise_reduction":null}}
            }}))
            .await?;
        upstream.wait_for("session.updated").await?;
        Ok(upstream)
    }
}
impl Upstream {
    async fn wait_for(&mut self, expected: &str) -> Result<(), Problem> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let event = self.recv().await?;
                if event["type"].as_str() == Some("error") {
                    return Err(interrupted());
                }
                if event["type"].as_str() == Some(expected) {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| interrupted())?
    }
    pub async fn send(&mut self, event: Value) -> Result<(), Problem> {
        self.sender.send(event).await
    }
    pub fn sender(&self) -> UpstreamSender {
        self.sender.clone()
    }
    pub async fn recv(&mut self) -> Result<Value, Problem> {
        loop {
            let data = match self.stream.next().await {
                Some(Ok(Message::Text(text))) => text.as_bytes().to_vec(),
                Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
                Some(Ok(Message::Ping(data))) => {
                    self.sender.write(Message::Pong(data)).await?;
                    continue;
                }
                Some(Ok(Message::Pong(_))) => continue,
                _ => return Err(interrupted()),
            };
            let value: Value = serde_json::from_slice(&data).map_err(|_| interrupted())?;
            if !value.is_object() {
                return Err(interrupted());
            }
            return Ok(value);
        }
    }
    pub async fn close(&mut self) -> Result<(), Problem> {
        self.sender.close().await
    }
}
impl UpstreamSender {
    pub async fn send(&self, event: Value) -> Result<(), Problem> {
        let data = serde_json::to_string(&event).map_err(|_| Problem::internal())?;
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(interrupted());
        }
        self.write(Message::Text(data.into())).await
    }
    async fn write(&self, message: Message) -> Result<(), Problem> {
        tokio::time::timeout(Duration::from_secs(3), async {
            self.sink
                .lock()
                .await
                .send(message)
                .await
                .map_err(|_| interrupted())
        })
        .await
        .map_err(|_| interrupted())?
    }
    pub async fn close(&self) -> Result<(), Problem> {
        self.write(Message::Close(None)).await
    }
}

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod tests;
