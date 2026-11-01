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
