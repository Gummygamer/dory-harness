//! The tool interface and the registry the agent calls through.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    hash::{DefaultHasher, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use dory_engine::{Error, Result, chat::ToolDef};
use serde_json::{Map, Value};

use crate::workspace::Workspace;

/// What a call can do, decided per call: a command's risk depends on the command, a file tool's
/// on its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    /// Reads inside the workspace.
    Read,
    /// Changes files inside the workspace.
    Write,
    /// Runs a program. Its reach is limited only by the sandbox.
    Execute,
    /// Sends text to a service outside the machine. Not `Read`: a read stays on this machine.
    Network,
}

/// What a tool returns to the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub truncated: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutcome {
            text: text.into(),
            is_error: false,
            truncated: false,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        let text = text.into();
        let text = if text.starts_with("error:") {
            text
        } else {
            format!("error: {text}")
        };
        ToolOutcome {
            text,
            is_error: true,
            truncated: false,
        }
    }
}

/// What tools may see of the session.
pub struct ToolCtx {
    pub workspace: Workspace,
    /// Effective command sandbox state shown in approval previews.
    pub command_sandbox: Option<String>,
    /// Files read or written this session, by hash. Edits and overwrites check against it.
    pub files: RefCell<FileLog>,
    /// Where full output of long commands is kept, so the model can read it back. Set by sessions.
    pub scratch: Option<PathBuf>,
    /// Raised by Ctrl-C (or a test). Running commands are killed when it is set.
    pub cancel: Arc<AtomicBool>,
    /// Searches started in the current task. Reset by the agent at the start of each turn.
    pub web_searches: Cell<usize>,
}

impl ToolCtx {
    pub fn new(workspace: Workspace) -> Self {
        ToolCtx::with_cancel(workspace, Arc::new(AtomicBool::new(false)))
    }

    /// A context that shares `cancel` with the agent, so one flag stops both.
    pub fn with_cancel(workspace: Workspace, cancel: Arc<AtomicBool>) -> Self {
        ToolCtx {
            workspace,
            command_sandbox: None,
            files: RefCell::new(FileLog::default()),
            scratch: None,
            cancel,
            web_searches: Cell::new(0),
        }
    }
}

/// The content of each file as the agent last saw it, by hash. A hash only lives as long as the
/// session, so `DefaultHasher` is enough.
#[derive(Default)]
pub struct FileLog {
    seen: HashMap<PathBuf, u64>,
}

impl FileLog {
    pub fn record(&mut self, path: &Path, bytes: &[u8]) {
        self.seen.insert(path.to_path_buf(), content_hash(bytes));
    }

    /// `None` if the agent has never seen this file.
    pub fn seen_hash(&self, path: &Path) -> Option<u64> {
        self.seen.get(path).copied()
    }
}

pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

pub trait Tool {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    /// Decided per call. Arguments are already checked against the schema when this is called.
    fn risk(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> Risk;
    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome;
    /// The command a call would run, for tools that run one. Matched against `--allow-command`.
    fn command_text(&self, _args: &Map<String, Value>) -> Option<String> {
        None
    }
    /// What an approval prompt shows for this call.
    fn summary(&self, args: &Map<String, Value>, _ctx: &ToolCtx) -> String {
        serde_json::to_string(args).unwrap_or_default()
    }
}

/// Tools in the order the model sees them.
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
    pub ctx: ToolCtx,
}

impl Registry {
    pub fn new(ctx: ToolCtx) -> Self {
        Registry {
            tools: Vec::new(),
            ctx,
        }
    }

    pub fn add(&mut self, tool: Box<dyn Tool>) -> Result<()> {
        if self.tools.iter().any(|t| t.name() == tool.name()) {
            return Err(Error::Tool(format!(
                "tool '{}' is registered twice",
                tool.name()
            )));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|t| t.def()).collect()
    }

    pub fn schemas(&self) -> HashMap<String, Value> {
        self.tools
            .iter()
            .map(|t| (t.name().to_string(), t.def().parameters))
            .collect()
    }

    pub fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// Finds the tool and checks its required arguments. Failures are results the model can read.
    pub fn prepare(
        &self,
        name: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<&dyn Tool, ToolOutcome> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolOutcome::err(format!("unknown tool '{name}'")))?;
        check_required(&tool.def(), args).map_err(ToolOutcome::err)?;
        Ok(tool)
    }

    /// Checks the arguments and runs the tool, without any permission check.
    pub fn call(&self, name: &str, args: &Map<String, Value>) -> ToolOutcome {
        match self.prepare(name, args) {
            Ok(tool) => tool.run(args, &self.ctx),
            Err(outcome) => outcome,
        }
    }
}

/// Missing and unknown arguments are reported by name, so the model can fix the call. This runs
/// before any approval is asked for, so a malformed call never reaches the user.
fn check_required(def: &ToolDef, args: &Map<String, Value>) -> std::result::Result<(), String> {
    let required = def.parameters.get("required").and_then(Value::as_array);
    for key in required.into_iter().flatten().filter_map(Value::as_str) {
        if !args.contains_key(key) {
            return Err(format!(
                "'{}' is missing required argument '{key}'",
                def.name
            ));
        }
    }
    if let Some(props) = def.parameters.get("properties").and_then(Value::as_object) {
        for key in args.keys() {
            if !props.contains_key(key) {
                return Err(format!("'{}' has no argument '{key}'", def.name));
            }
        }
    }
    Ok(())
}
