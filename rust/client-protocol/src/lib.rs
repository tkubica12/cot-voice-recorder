pub mod api;
pub mod auth;
pub mod delivery;
pub mod dictation_stream;
pub mod dictation_text;
pub mod history;
pub mod hotkeys;
pub mod legacy_checkpoint;
pub mod loopback;
pub mod network;
pub mod polishing;
pub mod realtime;
pub mod recovery;
pub mod runtime;
pub mod settings;
pub mod state;
pub mod websocket;

use std::fmt;

pub const MAX_AUTH_JSON_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidJson(&'static str),
    InvalidField(&'static str),
    InvalidCallback(&'static str),
    TooLarge,
    RandomUnavailable,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(context) => write!(f, "Invalid JSON in {context}."),
            Self::InvalidField(field) => write!(f, "Invalid or missing {field}."),
            Self::InvalidCallback(reason) => write!(f, "OAuth callback rejected: {reason}."),
            Self::TooLarge => f.write_str("Client protocol input exceeds supported bounds."),
            Self::RandomUnavailable => f.write_str("The operating system CSPRNG is unavailable."),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub(crate) fn json(data: &[u8], context: &'static str) -> Result<serde_json::Value, ProtocolError> {
    if data.len() > MAX_AUTH_JSON_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    serde_json::from_slice(data).map_err(|_| ProtocolError::InvalidJson(context))
}
