//! Decoding, output, player playlist and effects.
//!
//! This crate must never depend on Tauri or UI code. The `engine` feature holds
//! everything that touches audio, so a client of the protocol need not build it.

pub mod protocol;

#[cfg(feature = "engine")]
pub mod decoder;
#[cfg(feature = "engine")]
pub mod effects;
#[cfg(feature = "engine")]
pub mod engine;
#[cfg(feature = "engine")]
pub mod output;
#[cfg(feature = "engine")]
pub mod render;
#[cfg(feature = "engine")]
pub mod resample;
#[cfg(feature = "engine")]
pub mod source;
