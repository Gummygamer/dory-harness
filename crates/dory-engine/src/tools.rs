//! Tool definitions and execution behind an allowlist.
//!
//! A tools file is a JSON array. Each entry gives the model-facing `name`, `description`
//! and `parameters` schema, plus an optional `exec` argv. Only tools with an `exec` can run.
//! The model's arguments are sent as JSON on the command's stdin. Stdout is returned to the
//! model. Nothing else is ever executed, and calls to unknown tools are refused.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    chat::ToolDef,
    error::{Error, Result},
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub exec: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct ToolSet {
    pub entries: Vec<ToolEntry>,
}

impl ToolSet {
    pub fn load(path: &Path) -> Result<ToolSet> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io { path: path.to_path_buf(), source })?;
        let entries: Vec<ToolEntry> =
            serde_json::from_str(&text).map_err(|e| Error::Tool(format!("{}: {e}", path.display())))?;
        Ok(ToolSet { entries })
    }

    pub fn defs(&self) -> Vec<ToolDef> {
        self.entries
            .iter()
            .map(|t| ToolDef {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect()
    }

    pub fn schemas(&self) -> std::collections::HashMap<String, Value> {
        self.entries.iter().map(|t| (t.name.clone(), t.parameters.clone())).collect()
    }

    /// Runs a tool call. Errors become the tool's response text, so the model can recover.
    pub fn call(&self, name: &str, args: &Map<String, Value>) -> String {
        match self.try_call(name, args) {
            Ok(out) => out,
            Err(e) => format!("error: {e}"),
        }
    }

    fn try_call(&self, name: &str, args: &Map<String, Value>) -> Result<String> {
        let entry = self
            .entries
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::Tool(format!("unknown tool '{name}'")))?;
        let argv = entry
            .exec
            .as_ref()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| Error::Tool(format!("tool '{name}' has no exec entry")))?;
        check_required(entry, args)?;

        let input = serde_json::to_vec(args).map_err(|e| Error::Tool(e.to_string()))?;
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Tool(format!("cannot start {}: {e}", argv[0])))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input);
        }
        let mut stdout = child.stdout.take().ok_or_else(|| Error::Tool("no stdout".into()))?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });

        let start = Instant::now();
        loop {
            match child.try_wait().map_err(|e| Error::Tool(e.to_string()))? {
                Some(_) => break,
                None if start.elapsed() > TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::Tool(format!("'{name}' timed out after {}s", TIMEOUT.as_secs())));
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let mut out = reader.join().unwrap_or_default();
        out.truncate(MAX_OUTPUT);
        Ok(String::from_utf8_lossy(&out).trim_end().to_string())
    }
}

fn check_required(entry: &ToolEntry, args: &Map<String, Value>) -> Result<()> {
    let required = entry.parameters.get("required").and_then(Value::as_array);
    for key in required.into_iter().flatten().filter_map(Value::as_str) {
        if !args.contains_key(key) {
            return Err(Error::Tool(format!("'{}' is missing required argument '{key}'", entry.name)));
        }
    }
    Ok(())
}
