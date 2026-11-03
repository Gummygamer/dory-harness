//! Tools the agent provides itself. Each one checks its paths through the workspace.

use dory_engine::chat::ToolDef;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::tool::{Tool, ToolOutcome};

pub mod command;
pub mod read;
pub mod write;

/// Largest file any file tool will read, in bytes.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Output a tool returns, in bytes. Anything beyond it is cut and the cut is said.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Parses a tool's arguments. Unknown keys are an error, so a misnamed argument is reported
/// instead of silently ignored.
pub fn parse_args<T: DeserializeOwned>(tool: &str, args: &Map<String, Value>) -> Result<T, ToolOutcome> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| ToolOutcome::err(format!("{tool}: {e}")))
}

/// Whether the first bytes of a file look binary.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// A short name for a few common binary formats, from their first bytes.
pub fn binary_type(bytes: &[u8]) -> &'static str {
    const KNOWN: &[(&[u8], &str)] = &[
        (b"\x89PNG", "PNG image"),
        (b"\xff\xd8\xff", "JPEG image"),
        (b"GIF8", "GIF image"),
        (b"%PDF", "PDF document"),
        (b"\x7fELF", "ELF executable"),
        (b"PK\x03\x04", "zip archive"),
        (b"\x1f\x8b", "gzip archive"),
        (b"SQLite format 3", "SQLite database"),
    ];
    KNOWN.iter().find(|(magic, _)| bytes.starts_with(magic)).map(|(_, name)| *name).unwrap_or("unknown binary data")
}

/// Cuts `s` to at most `max` bytes on a character boundary.
pub fn cut_bytes(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    true
}

/// Builds a tool definition from its name, description and JSON-schema properties.
pub fn def(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: Some(description.to_string()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
        }),
    }
}

/// Returns the path string as the model wrote it, for messages.
pub fn shown(path: &str) -> &str {
    if path.is_empty() { "." } else { path }
}

/// The built-in read-only tools.
pub fn read_tools() -> Vec<Box<dyn Tool>> {
    read::tools()
}

/// The built-in tools that change files.
pub fn write_tools() -> Vec<Box<dyn Tool>> {
    write::tools()
}
