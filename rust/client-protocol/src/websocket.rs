use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::api::{ApiClient, ApiErrorKind, NegotiateResponse};
use crate::realtime::{Frame, TranscriptCompleted};
use crate::runtime::TokenPersistence;
use crate::state::AuthState;

const SUBPROTOCOL: &str = "json.webpubsub.azure.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeState {
    Stopped,
    Suspended,
    Connecting,
    Connected,
    Reconnecting,
    SignInRequired,
    NotConfigured,
}

#[derive(Clone, Copy, Debug, Default)]
struct Control {
    stopped: bool,
    suspended: bool,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeError {
    AlreadyRunning,
    InvalidAccess,
    Connect,
    Receive,
    Send,
    ConsumerClosed,
    RandomUnavailable,
}

impl fmt::Display for RealtimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Realtime connection failure: {self:?}.")
    }
}

impl std::error::Error for RealtimeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionEnd {
    PeerClosed,
    Renew,
    ControlChanged,
}

pub struct RealtimeClient<P: TokenPersistence> {
    api: Arc<ApiClient<P>>,
    version: String,
    control: watch::Sender<Control>,
    state: watch::Sender<RealtimeState>,
    events: mpsc::Sender<TranscriptCompleted>,
    running: AtomicBool,
}

struct RunGuard<'a> {
    running: &'a AtomicBool,
    state: &'a watch::Sender<RealtimeState>,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.state.send_replace(RealtimeState::Stopped);
    }
}

impl<P: TokenPersistence> RealtimeClient<P> {
    pub fn new(
        api: Arc<ApiClient<P>>,
        version: String,
    ) -> (Self, mpsc::Receiver<TranscriptCompleted>) {
        let (control, _) = watch::channel(Control::default());
        let (state, _) = watch::channel(RealtimeState::Stopped);
        let (events, receiver) = mpsc::channel(32);
        (
            Self {
                api,
                version,
                control,
                state,
                events,
                running: AtomicBool::new(false),
            },
            receiver,
        )
    }

    pub fn state(&self) -> watch::Receiver<RealtimeState> {
        self.state.subscribe()
    }
    pub fn suspend(&self) {
        self.control.send_modify(|value| {
            value.suspended = true;
            value.generation = value.generation.wrapping_add(1);
        });
    }
    pub fn resume(&self) {
        self.control.send_modify(|value| {
            value.suspended = false;
            value.generation = value.generation.wrapping_add(1);
        });
    }
    pub fn reconnect(&self) {
        self.control
            .send_modify(|value| value.generation = value.generation.wrapping_add(1));
    }
    pub fn stop(&self) {
        self.control.send_modify(|value| {
            value.stopped = true;
            value.generation = value.generation.wrapping_add(1);
        });
    }

    pub async fn run(&self) -> Result<(), RealtimeError> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RealtimeError::AlreadyRunning);
        }
        let _run_guard = RunGuard {
            running: &self.running,
            state: &self.state,
        };
        let mut control = self.control.subscribe();
        let mut auth = self.api.auth_state();
        let mut attempt = 0u32;
        loop {
            let command = *control.borrow_and_update();
            if command.stopped {
                self.state.send_replace(RealtimeState::Stopped);
                return Ok(());
            }
            let auth_state = *auth.borrow_and_update();
            if command.suspended || auth_state != AuthState::SignedIn {
                self.state.send_replace(if command.suspended {
                    RealtimeState::Suspended
                } else if auth_state == AuthState::NotConfigured {
                    RealtimeState::NotConfigured
                } else {
                    RealtimeState::SignInRequired
                });
                tokio::select! {
                    changed = control.changed() => if changed.is_err() { return Ok(()); },
                    changed = auth.changed() => if changed.is_err() { return Ok(()); },
                }
                attempt = 0;
                continue;
            }
            self.state.send_replace(RealtimeState::Connecting);
            let access = tokio::select! {
                response = self.api.negotiate(&self.version) => response,
                _ = control.changed() => continue,
                _ = auth.changed() => continue,
            };
            let delay = match access {
                Ok(access) => {
                    let outcome = receive_connection(
                        &access,
                        &mut control,
                        &mut auth,
                        &self.state,
                        &self.events,
                        false,
                    )
                    .await;
                    match outcome {
                        Ok(ConnectionEnd::ControlChanged | ConnectionEnd::Renew) => {
                            attempt = 0;
                            Duration::ZERO
                        }
                        Ok(ConnectionEnd::PeerClosed) => {
                            attempt = attempt.saturating_add(1);
                            reconnect_delay(attempt)?
                        }
                        Err(RealtimeError::ConsumerClosed) => {
                            self.state.send_replace(RealtimeState::Stopped);
                            return Err(RealtimeError::ConsumerClosed);
                        }
                        Err(error) => {
                            eprintln!("Realtime: {error}");
                            attempt = attempt.saturating_add(1);
                            reconnect_delay(attempt)?
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.kind,
                        ApiErrorKind::Unauthorized | ApiErrorKind::Forbidden
                    ) =>
                {
                    self.state.send_replace(RealtimeState::SignInRequired);
                    Duration::from_secs(300)
                }
                Err(error) => {
                    eprintln!("Realtime negotiation: {error}");
                    attempt = attempt.saturating_add(1);
                    reconnect_delay(attempt)?
                }
            };
            if !delay.is_zero() {
                if *self.state.borrow() != RealtimeState::SignInRequired {
                    self.state.send_replace(RealtimeState::Reconnecting);
                }
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = control.changed() => { attempt = 0; },
                    _ = auth.changed() => { attempt = 0; },
                }
            }
        }
    }
}

fn reconnect_delay(attempt: u32) -> Result<Duration, RealtimeError> {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|_| RealtimeError::RandomUnavailable)?;
    let ratio = f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX);
    let ceiling = (2f64.powi(attempt.saturating_sub(1).min(30) as i32)).min(60.0);
    Ok(Duration::from_secs_f64(ceiling * (0.5 + ratio * 0.5)))
}

async fn receive_connection(
    access: &NegotiateResponse,
    control: &mut watch::Receiver<Control>,
    auth: &mut watch::Receiver<AuthState>,
    state: &watch::Sender<RealtimeState>,
    events: &mpsc::Sender<TranscriptCompleted>,
    allow_test_loopback: bool,
) -> Result<ConnectionEnd, RealtimeError> {
    let uri = url::Url::parse(&access.url).map_err(|_| RealtimeError::InvalidAccess)?;
    let test = allow_test_loopback
        && uri.scheme() == "ws"
        && uri.host_str() == Some("127.0.0.1")
        && uri.port().is_some();
    if (!test && uri.scheme() != "wss")
        || uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
    {
        return Err(RealtimeError::InvalidAccess);
    }
    let lifetime = access.expires_at - time::OffsetDateTime::now_utc() - time::Duration::minutes(2);
    if lifetime <= time::Duration::ZERO || lifetime > time::Duration::days(1) {
        return Err(RealtimeError::InvalidAccess);
    }
    let mut request = access
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| RealtimeError::InvalidAccess)?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    let config = WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(1024 * 1024)
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let (mut socket, _) = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(request, Some(config), true)) =>
            result.map_err(|_| RealtimeError::Connect)?.map_err(|_| RealtimeError::Connect)?,
        _ = control.changed() => return Ok(ConnectionEnd::ControlChanged),
        _ = auth.changed() => return Ok(ConnectionEnd::ControlChanged),
    };
    state.send_replace(RealtimeState::Connected);
    let renewal =
        tokio::time::sleep(Duration::try_from(lifetime).map_err(|_| RealtimeError::InvalidAccess)?);
    tokio::pin!(renewal);
    let mut keepalive = tokio::time::interval(Duration::from_secs(30));
    let end = loop {
        tokio::select! {
            _ = control.changed() => break ConnectionEnd::ControlChanged,
            _ = auth.changed() => break ConnectionEnd::ControlChanged,
            _ = &mut renewal => break ConnectionEnd::Renew,
            _ = keepalive.tick() => {
                socket.send(Message::Ping(Vec::new().into())).await.map_err(|_| RealtimeError::Send)?;
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let frame = match Frame::parse(text.as_bytes()) {
                            Ok(frame) => frame,
                            Err(error) => { eprintln!("Realtime frame rejected: {error}"); continue; }
                        };
                        if frame.is_disconnected() { break ConnectionEnd::PeerClosed; }
                        match frame.completed() {
                            Ok(Some(event)) => {
                                tokio::select! {
                                    result = events.send(event) => result.map_err(|_| RealtimeError::ConsumerClosed)?,
                                    _ = control.changed() => break ConnectionEnd::ControlChanged,
                                    _ = auth.changed() => break ConnectionEnd::ControlChanged,
                                    _ = &mut renewal => break ConnectionEnd::Renew,
                                }
                            }
                            Ok(None) => {},
                            Err(error) => eprintln!("Realtime event rejected: {error}"),
                        }
                    }
                    Some(Ok(Message::Ping(bytes))) => socket.send(Message::Pong(bytes)).await.map_err(|_| RealtimeError::Send)?,
                    Some(Ok(Message::Close(_))) | None => break ConnectionEnd::PeerClosed,
                    Some(Ok(_)) => {},
                    Some(Err(_)) => return Err(RealtimeError::Receive),
                }
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(3), socket.close(None)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => eprintln!("Realtime close did not complete cleanly."),
    }
    Ok(end)
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
