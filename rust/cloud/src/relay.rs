use crate::problem::Problem;
use crate::streaming::Upstream;
use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

const PREFIX: &str = "conversation.item.input_audio_transcription.";
const MAX_TEXT_CHARS: usize = 1_048_576;
const MAX_COMMITS: usize = 32;

fn invalid() -> Problem {
    Problem::validation("Invalid streaming frame or upstream confirmation sequence.")
}
pub struct RelayState {
    start_byte: u64,
    pub cursor: u64,
    committed_through: u64,
    pending: VecDeque<u64>,
    order: VecDeque<u64>,
    by_item: HashMap<String, u64>,
    completed: HashMap<u64, (String, String)>,
    pub finished: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    #[serde(rename = "type")]
    kind: String,
}

impl RelayState {
    pub fn new(start_byte: u64) -> Self {
        Self {
            start_byte,
            cursor: start_byte,
            committed_through: start_byte,
            pending: VecDeque::new(),
            order: VecDeque::new(),
            by_item: HashMap::new(),
            completed: HashMap::new(),
            finished: false,
        }
    }
    pub fn audio(&mut self, audio: &[u8]) -> Result<Value, Problem> {
        if self.finished || audio.is_empty() || audio.len() > 16384 || audio.len() % 2 != 0 {
            return Err(invalid());
        }
        let cursor = self
            .cursor
            .checked_add(audio.len() as u64)
            .ok_or_else(invalid)?;
        if cursor - self.start_byte > 32000 * 3600 {
            return Err(invalid());
        }
        self.cursor = cursor;
        Ok(
            serde_json::json!({"type":"input_audio_buffer.append","audio":base64::engine::general_purpose::STANDARD.encode(audio)}),
        )
    }
    pub fn control(&mut self, text: &str) -> Result<Option<Value>, Problem> {
        if self.finished || text.chars().count() > 256 {
            return Err(invalid());
        }
        let control: Control = serde_json::from_str(text).map_err(|_| invalid())?;
        if !matches!(control.kind.as_str(), "commit" | "finish") {
            return Err(invalid());
        }
        let message = if self.cursor != self.committed_through {
            if self.order.len() >= MAX_COMMITS {
                return Err(invalid());
            }
            self.pending.push_back(self.cursor);
            self.order.push_back(self.cursor);
            self.committed_through = self.cursor;
            Some(serde_json::json!({"type":"input_audio_buffer.commit"}))
        } else {
            None
        };
        if control.kind == "finish" {
            self.finished = true;
        }
        Ok(message)
    }
    pub fn event(&mut self, event: Value) -> Result<Vec<Value>, Problem> {
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "error" || kind == format!("{PREFIX}failed") {
            return Err(invalid());
        }
        if !matches!(kind, "input_audio_buffer.committed")
            && !["delta", "intermediate", "completed"]
                .iter()
                .any(|suffix| kind == format!("{PREFIX}{suffix}"))
        {
            return Ok(Vec::new());
        }
        let item = event["item_id"]
            .as_str()
            .filter(|item| item.len() <= 1024)
            .ok_or_else(invalid)?;
        if kind.ends_with(".delta") || kind.ends_with(".intermediate") {
            let field = if kind.ends_with(".delta") {
                "delta"
            } else {
                "intermediate"
            };
            let text = event[field]
                .as_str()
                .filter(|text| text.chars().count() <= MAX_TEXT_CHARS)
                .ok_or_else(invalid)?;
            return Ok(vec![
                serde_json::json!({"type":field,"item_id":item,"text":text}),
            ]);
        }
        if kind == "input_audio_buffer.committed" {
            if self.by_item.contains_key(item) {
                return Err(invalid());
            }
            let end = self.pending.pop_front().ok_or_else(invalid)?;
            self.by_item.insert(item.into(), end);
            return Ok(Vec::new());
        }
        let text = event["transcript"]
            .as_str()
            .filter(|text| text.chars().count() <= MAX_TEXT_CHARS)
            .ok_or_else(invalid)?;
        let end = self.by_item.remove(item).ok_or_else(invalid)?;
        self.completed.insert(end, (item.into(), text.into()));
        let mut messages = Vec::new();
        while let Some(end) = self.order.front().copied() {
            let Some((item, text)) = self.completed.remove(&end) else {
                break;
            };
            self.order.pop_front();
            messages.push(
                serde_json::json!({"type":"confirmed","item_id":item,"byte_end":end,"text":text}),
            );
        }
        Ok(messages)
    }
    pub fn done(&self) -> Option<Value> {
        if self.finished && self.order.is_empty() {
            Some(serde_json::json!({"type":"done","byte_end":self.cursor}))
        } else {
            None
        }
    }
}
pub(crate) async fn send(client: &mut WebSocket, event: Value) -> Result<(), Problem> {
    let message = serde_json::to_string(&event).map_err(|_| Problem::internal())?;
    tokio::time::timeout(
        Duration::from_secs(3),
        client.send(Message::Text(message.into())),
    )
    .await
    .map_err(|_| invalid())?
    .map_err(|_| invalid())
}
pub async fn relay(
    client: &mut WebSocket,
    upstream: &mut Upstream,
    start_byte: u64,
) -> Result<(), Problem> {
    let mut state = RelayState::new(start_byte);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(55 * 60);
    let mut drain_deadline = deadline;
    loop {
        if let Some(event) = state.done() {
            return send(client, event).await;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Err(invalid()),
            _ = tokio::time::sleep_until(drain_deadline), if state.finished => return Err(invalid()),
            message = client.recv() => {
                match message {
                    Some(Ok(Message::Binary(audio))) => upstream.send(state.audio(&audio)?).await?,
                    Some(Ok(Message::Text(control))) => {
                        if let Some(event) = state.control(control.as_str())? { upstream.send(event).await?; }
                        if state.finished { drain_deadline = tokio::time::Instant::now() + Duration::from_secs(30); }
                    }
                    Some(Ok(Message::Ping(_)|Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(_)) => return Err(invalid()),
                }
            }
            event = upstream.recv() => {
                for message in state.event(event?)? { send(client, message).await?; }
            }
        }
    }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
