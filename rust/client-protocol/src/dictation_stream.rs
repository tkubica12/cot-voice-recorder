use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{Mutex, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::api::{ApiClient, ApiError, ApiErrorKind, classify_status};
use crate::runtime::{AuthRuntime, AuthorizedSession, TokenPersistence};
use crate::state::AuthState;

#[cfg(test)]
#[path = "dictation_stream_tests.rs"]
mod tests;

const DEADLINE: Duration = Duration::from_secs(25);
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 16 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Sender = Arc<Mutex<SplitSink<Socket, Message>>>;

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DictationMessage {
    Ready {
        protocol: u32,
        start_byte: u64,
    },
    Delta {
        item_id: String,
        text: String,
    },
    Intermediate {
        item_id: String,
        text: String,
    },
    Confirmed {
        item_id: String,
        text: String,
        byte_end: u64,
    },
    Done {
        byte_end: u64,
    },
    Error {
        #[serde(default)]
        code: Option<String>,
    },
}

impl fmt::Debug for DictationMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DictationMessage(<redacted>)")
    }
}

impl DictationMessage {
    pub fn parse(data: &[u8]) -> Result<Self, ApiError> {
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(invalid());
        }
        let message: Self = serde_json::from_slice(data).map_err(|_| invalid())?;
        match &message {
            Self::Error { code } => {
                let status = match code.as_deref() {
                    Some("unauthorized") => 401,
                    Some("forbidden") => 403,
                    Some("invalid_request") => 422,
                    Some("too_many_requests") => 429,
                    _ => 503,
                };
                return Err(failure(classify_status(status), Some(status), false));
            }
            Self::Delta { item_id, text }
            | Self::Intermediate { item_id, text }
            | Self::Confirmed { item_id, text, .. }
                if item_id.len() > 1024 || text.encode_utf16().count() > 1_048_576 =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        match &message {
            Self::Confirmed { byte_end, .. } | Self::Done { byte_end }
                if *byte_end > i64::MAX as u64 || byte_end % 2 != 0 =>
            {
                return Err(invalid());
            }
            Self::Ready { start_byte, .. }
                if *start_byte > i64::MAX as u64 || start_byte % 2 != 0 =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        Ok(message)
    }
}

pub struct DictationConnection {
    sender: DictationSender,
    receiver: DictationReceiver,
}

impl DictationConnection {
    pub fn split(self) -> (DictationSender, DictationReceiver) {
        (self.sender, self.receiver)
    }
}

#[derive(Clone)]
pub struct DictationSender {
    sink: Sender,
    auth: watch::Receiver<AuthState>,
}

pub struct DictationReceiver {
    stream: SplitStream<Socket>,
    sender: DictationSender,
    auth: watch::Receiver<AuthState>,
    keepalive: tokio::time::Interval,
}

fn failure(kind: ApiErrorKind, status: Option<u16>, invalid: bool) -> ApiError {
    ApiError {
        kind,
        status,
        problem: None,
        response_invalid: invalid,
    }
}

fn invalid() -> ApiError {
    failure(ApiErrorKind::Unexpected, None, true)
}

fn auth_changed(auth: &watch::Receiver<AuthState>) -> ApiError {
    if *auth.borrow() == AuthState::SignedIn {
        failure(ApiErrorKind::Retryable, None, false)
    } else {
        failure(ApiErrorKind::Unauthorized, Some(401), false)
    }
}

fn ensure_auth(auth: &watch::Receiver<AuthState>) -> Result<(), ApiError> {
    match auth.has_changed() {
        Ok(false) if *auth.borrow() == AuthState::SignedIn => Ok(()),
        Ok(_) => Err(auth_changed(auth)),
        Err(_) => Err(failure(ApiErrorKind::Unauthorized, Some(401), false)),
    }
}

fn transport_error(error: Error) -> ApiError {
    match error {
        Error::Http(response) => failure(
            classify_status(response.status().as_u16()),
            Some(response.status().as_u16()),
            false,
        ),
        Error::Capacity(_) | Error::Protocol(_) | Error::Utf8(_) => invalid(),
        _ => failure(ApiErrorKind::Retryable, None, false),
    }
}

pub(crate) async fn connect<P: TokenPersistence>(
    mut base: url::Url,
    auth: Arc<AuthRuntime<P>>,
    language: &str,
    start_byte: u64,
    expected_email: Option<&str>,
) -> Result<DictationConnection, ApiError> {
    if !matches!(language, "auto" | "cs" | "en")
        || start_byte % 2 != 0
        || start_byte > i64::MAX as u64
    {
        return Err(failure(ApiErrorKind::BadRequest, None, false));
    }
    let loopback = (cfg!(test) || cfg!(feature = "loopback-fixtures"))
        && base.scheme() == "http"
        && base.host_str() == Some("127.0.0.1")
        && base.port().is_some();
    if (!loopback && base.scheme() != "https")
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(failure(ApiErrorKind::BadRequest, None, false));
    }
    base.set_scheme(if loopback { "ws" } else { "wss" })
        .map_err(|_| invalid())?;
    base.set_path("/v1/dictation/stream");
    base.set_query(None);
    base.query_pairs_mut()
        .append_pair("language", language)
        .append_pair("start_byte", &start_byte.to_string());
    for attempt in 0..2 {
        if let Some(expected) = expected_email {
            if auth.current_email().await.as_deref() != Some(expected) {
                return Err(failure(ApiErrorKind::Unauthorized, Some(401), false));
            }
        }
        let credential = auth
            .authorized_session()
            .await
            .map_err(ApiClient::<P>::auth_failure)?
            .ok_or_else(|| failure(ApiErrorKind::Unauthorized, Some(401), false))?;
        if expected_email.is_some_and(|expected| credential.email.as_deref() != Some(expected)) {
            return Err(failure(ApiErrorKind::Unauthorized, Some(401), false));
        }
        let outcome = open(&base, credential, start_byte).await;
        match outcome {
            Err(error) if error.kind == ApiErrorKind::Unauthorized && attempt == 0 => {
                if auth
                    .force_refresh()
                    .await
                    .map_err(ApiClient::<P>::auth_failure)?
                    .is_none()
                {
                    return Err(error);
                }
            }
            result => return result,
        }
    }
    Err(failure(ApiErrorKind::Unauthorized, Some(401), false))
}

async fn open(
    uri: &url::Url,
    mut credential: AuthorizedSession,
    start_byte: u64,
) -> Result<DictationConnection, ApiError> {
    ensure_auth(&credential.state)?;
    let mut request = uri.as_str().into_client_request().map_err(|_| invalid())?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", credential.token)).map_err(|_| invalid())?,
    );
    let config = WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(64 * 1024)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let until = tokio::time::Instant::now() + DEADLINE;
    let (mut socket, _) = tokio::select! {
        result = tokio::time::timeout_at(until, tokio_tungstenite::connect_async_with_config(request, Some(config), true)) =>
            result.map_err(|_| failure(ApiErrorKind::Retryable, None, false))?.map_err(transport_error)?,
        _ = credential.state.changed() => return Err(auth_changed(&credential.state)),
    };
    let ready = tokio::select! {
        result = tokio::time::timeout_at(until, async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => return DictationMessage::parse(text.as_bytes()),
                    Some(Ok(Message::Ping(bytes))) => socket.send(Message::Pong(bytes)).await.map_err(transport_error)?,
                    Some(Ok(Message::Pong(_))) => {},
                    Some(Err(error)) => return Err(transport_error(error)),
                    _ => return Err(invalid()),
                }
            }
        }) => result.map_err(|_| failure(ApiErrorKind::Retryable, None, false))??,
        _ = credential.state.changed() => return Err(auth_changed(&credential.state)),
    };
    if !matches!(ready, DictationMessage::Ready { protocol: 1, start_byte: value } if value == start_byte)
    {
        return Err(invalid());
    }
    ensure_auth(&credential.state)?;
    let (sink, stream) = socket.split();
    let sender = DictationSender {
        sink: Arc::new(Mutex::new(sink)),
        auth: credential.state.clone(),
    };
    let receiver = DictationReceiver {
        stream,
        sender: sender.clone(),
        auth: credential.state,
        keepalive: tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(15),
            Duration::from_secs(15),
        ),
    };
    Ok(DictationConnection { sender, receiver })
}

impl DictationSender {
    async fn send(&self, message: Message) -> Result<(), ApiError> {
        let mut auth = self.auth.clone();
        ensure_auth(&auth)?;
        tokio::select! {
            result = tokio::time::timeout(DEADLINE, async {
                self.sink.lock().await.send(message).await.map_err(transport_error)
            }) => result.map_err(|_| failure(ApiErrorKind::Retryable, None, false))?,
            _ = auth.changed() => Err(auth_changed(&auth)),
        }
    }

    pub async fn audio(&self, pcm: &[u8]) -> Result<(), ApiError> {
        if pcm.is_empty() || pcm.len() > MAX_PACKET_BYTES || pcm.len() % 2 != 0 {
            return Err(failure(ApiErrorKind::BadRequest, None, false));
        }
        self.send(Message::Binary(pcm.to_vec().into())).await
    }

    pub async fn commit(&self, finish: bool) -> Result<(), ApiError> {
        self.send(Message::Text(
            if finish {
                r#"{"type":"finish"}"#
            } else {
                r#"{"type":"commit"}"#
            }
            .into(),
        ))
        .await
    }
}

impl DictationReceiver {
    pub async fn receive(&mut self) -> Result<DictationMessage, ApiError> {
        ensure_auth(&self.auth)?;
        let until = tokio::time::Instant::now() + DEADLINE;
        let deadline = tokio::time::sleep_until(until);
        tokio::pin!(deadline);
        loop {
            let message = tokio::select! {
                _ = &mut deadline => return Err(failure(ApiErrorKind::Retryable, None, false)),
                _ = self.auth.changed() => return Err(auth_changed(&self.auth)),
                _ = self.keepalive.tick() => {
                    tokio::time::timeout_at(until, self.sender.send(Message::Pong(Vec::new().into())))
                        .await.map_err(|_| failure(ApiErrorKind::Retryable, None, false))??;
                    continue;
                }
                message = self.stream.next() => message,
            };
            ensure_auth(&self.auth)?;
            match message {
                Some(Ok(Message::Text(text))) => {
                    let message = DictationMessage::parse(text.as_bytes())?;
                    if matches!(message, DictationMessage::Ready { .. }) {
                        return Err(invalid());
                    }
                    return Ok(message);
                }
                Some(Ok(Message::Ping(bytes))) => {
                    tokio::time::timeout_at(until, self.sender.send(Message::Pong(bytes)))
                        .await
                        .map_err(|_| failure(ApiErrorKind::Retryable, None, false))??;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Err(error)) => return Err(transport_error(error)),
                _ => return Err(failure(ApiErrorKind::Retryable, None, false)),
            }
        }
    }
}
