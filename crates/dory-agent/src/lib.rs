//! The agent: a loop that lets the model read and change files and run commands.
//!
//! Layers:
//! - [`backend`]: what generates text. [`backend::EngineBackend`] is the real model;
//!   [`scripted::ScriptedBackend`] replays canned replies, so the loop is tested with no GPU.
//! - [`agent`]: the loop. It renders, generates, parses, runs tools and repeats. It never prints;
//!   it reports everything through [`agent::AgentEvent`], and the front end decides what to show.
//! - [`tools`]: the tool registry and the output sanitizer.
//! - [`process`]: bounded process execution, shared by external tools and `run_command`.
//! - [`prompt`]: the fixed system prompt for agent sessions.

pub mod agent;
pub mod builtin;
pub mod compact;
pub mod backend;
pub mod policy;
pub mod process;
pub mod prompt;
pub mod sandbox;
pub mod session;
pub mod scripted;
pub mod tool;
pub mod tools;
pub mod web;
pub mod workspace;

pub use agent::{Agent, AgentConfig, AgentEvent, Reason, TurnReport};
pub use backend::Backend;
pub use tool::{Registry, Risk, Tool, ToolCtx, ToolOutcome};
