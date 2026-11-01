#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod platform;

#[cfg(windows)]
mod clipboard;

#[cfg(windows)]
mod client;

#[cfg(windows)]
pub use client::ClientOptions;

#[cfg(windows)]
pub use platform::*;

#[cfg(windows)]
mod recovery_store;

#[cfg(windows)]
pub use recovery_store::{
    DEFAULT_RECOVERY_QUOTA, NativeRecoveryInfo, NativeRecoveryJournal, NativeRecoveryStore,
};

#[cfg(windows)]
mod legacy_session;

#[cfg(windows)]
pub use legacy_session::{
    BatchBackend, BoundBatchBackend, DictationProgress, LegacySession, SessionError,
};

#[cfg(windows)]
mod streaming_session;

#[cfg(windows)]
pub use streaming_session::{AudioSender, EventReceiver, StreamingBackend, StreamingSession};

#[cfg(windows)]
mod audit_outbox;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod compatible_instance;
#[cfg(windows)]
mod dpi;
#[cfg(windows)]
pub use compatible_instance::{CompatibleInstance, InstanceEvent};
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod dictation;
#[cfg(windows)]
mod dictation_ui;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod microphone;
#[cfg(windows)]
mod settings_ui;
