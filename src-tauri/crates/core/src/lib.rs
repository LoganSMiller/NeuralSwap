//! Host-agnostic core for NeuralSwap.
//!
//! Nothing here knows about Tauri, a window, or a frontend. That is deliberate
//! and load-bearing: these are the parts that decide what gets written into
//! somebody's game folder, and they are tested against the behavioural vectors
//! in `spec/` rather than through the application.

// `expect` and `panic` are correct in a test: a broken fixture should abort
// the run, loudly. The lints stay strict for everything that ships.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod bytes;
pub mod components;
pub mod error;
pub mod fsx;
pub mod hash;
pub mod install;
pub mod jobs;
pub mod library;
pub mod pe;
pub mod platform;
pub mod scan;
pub mod settings;
pub mod zip;

pub use error::{Code, Error, Result};
