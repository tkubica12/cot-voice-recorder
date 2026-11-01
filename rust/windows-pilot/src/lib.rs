#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod platform;

#[cfg(windows)]
pub use platform::*;
