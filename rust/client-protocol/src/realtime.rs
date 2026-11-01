use std::collections::{HashSet, VecDeque};
use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

use crate::{ProtocolError, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Unknown,
    System,
    Message,
    Ack,
}

#[derive(Clone)]
pub struct Frame {
    pub kind: FrameKind,
    pub system_event: Option<String>,
    pub from: Option<String>,
    pub group: Option<String>,
    pub data_type: Option<String>,
    pub user_id: Option<String>,
    pub connection_id: Option<String>,
    payload: Option<Value>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &self.kind)
            .field("payload", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Deserialize)]
pub struct TranscriptCompleted {
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub transcript_id: String,
    #[serde(default)]
    pub recording_id: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub preview: String,
}

impl fmt::Debug for TranscriptCompleted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TranscriptCompleted(<redacted>)")
    }
}

impl Frame {
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        let root = json(data, "Web PubSub frame")?;
        let mut result = Self {
            kind: FrameKind::Unknown,
            system_event: None,
            from: None,
            group: None,
            data_type: None,
            user_id: None,
            connection_id: None,
            payload: None,
        };
        if !root.is_object() {
            return Ok(result);
        }
        let string = |name| root.get(name).and_then(Value::as_str).map(str::to_owned);
        let kind = string("type");
        match kind.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("system") => {
                result.kind = FrameKind::System;
                result.system_event = string("event");
                result.user_id = string("userId");
                result.connection_id = string("connectionId");
            }
            Some("ack") => result.kind = FrameKind::Ack,
            Some("message") => {
                result.kind = FrameKind::Message;
                result.from = string("from");
                result.group = string("group");
                result.data_type = Some(string("dataType").unwrap_or_else(|| "text".into()));
                result.user_id = string("fromUserId");
                result.payload = match root.get("data") {
                    Some(value @ (Value::Object(_) | Value::Array(_))) => Some(value.clone()),
                    Some(Value::String(text)) => Some(Value::String(text.clone())),
                    _ => None,
                };
            }
            None if root.get("event").is_some() => {
                result.kind = FrameKind::Message;
                result.from = Some("server".into());
                result.data_type = Some("json".into());
                result.payload = Some(root);
            }
            _ => {}
        }
        Ok(result)
    }

    pub fn is_connected(&self) -> bool {
        self.kind == FrameKind::System
            && self
                .system_event
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("connected"))
    }

    pub fn is_disconnected(&self) -> bool {
        self.kind == FrameKind::System
            && self
                .system_event
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("disconnected"))
    }

    pub fn completed(&self) -> Result<Option<TranscriptCompleted>, ProtocolError> {
        if self.kind != FrameKind::Message
            || self.data_type.as_deref().is_some_and(|value| {
                !value.eq_ignore_ascii_case("json") && !value.eq_ignore_ascii_case("text")
            })
        {
            return Ok(None);
        }
        let Some(payload) = &self.payload else {
            return Ok(None);
        };
        let value = match payload {
            Value::String(text) => json(text.as_bytes(), "Web PubSub text payload")?,
            value => value.clone(),
        };
        let completed: TranscriptCompleted = serde_json::from_value(value)
            .map_err(|_| ProtocolError::InvalidJson("transcript.completed payload"))?;
        if completed.event != "transcript.completed"
            || completed.event_id.is_empty()
            || completed.transcript_id.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(completed))
    }
}

pub struct EventDeduplicator {
    capacity: usize,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl EventDeduplicator {
    pub fn new(capacity: usize) -> Result<Self, ProtocolError> {
        if capacity == 0 || capacity > 100_000 {
            return Err(ProtocolError::InvalidField("event dedupe capacity"));
        }
        Ok(Self {
            capacity,
            seen: HashSet::new(),
            order: VecDeque::new(),
        })
    }

    pub fn mark(&mut self, event_id: Option<&str>) -> Result<bool, ProtocolError> {
        let Some(id) = event_id.filter(|value| !value.is_empty()) else {
            return Ok(true);
        };
        if id.len() > 1024 {
            return Err(ProtocolError::TooLarge);
        }
        if !self.seen.insert(id.to_owned()) {
            return Ok(false);
        }
        self.order.push_back(id.to_owned());
        if self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        Ok(true)
    }

    pub fn has_seen(&self, event_id: &str) -> bool {
        self.seen.contains(event_id)
    }
    pub fn count(&self) -> usize {
        self.seen.len()
    }
    pub fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}
