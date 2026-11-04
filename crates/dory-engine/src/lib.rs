//! Rust harness for the Nemotron Dory model on the llama.cpp PR #30306 fork.
//!
//! Layers, bottom up:
//! - [`verify`]: pins the model checksum and fork commit.
//! - [`model`]: RAII wrappers around `llama_model` and `llama_context`.
//! - [`engine`]: a worker thread that owns the model, streams generation, and manages the KV cache.
//! - [`chat`]: renders messages into the Dory prompt format.
//! - [`parse`]: splits streamed output into reasoning, content and tool calls.
//!
//! Tools and process execution live in `dory-agent`.

pub mod chat;
pub mod engine;
pub mod error;
pub mod log;
pub mod model;
pub mod parse;
pub mod prefill;
pub mod verify;

pub use engine::{
    CancelHandle, Engine, EngineOptions, Event, Finish, GenParams, GenStats, Generation, ModelInfo,
    tokenize_model,
};
pub use error::{Error, Result};
pub use model::{CacheMode, LoadOptions};
pub use prefill::PrefillSchedule;
