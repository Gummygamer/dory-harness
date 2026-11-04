//! `run_command`: runs a shell command in the workspace, under the sandbox when one is active.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use rillwright_engine::chat::ToolDef;

use super::{MAX_OUTPUT_BYTES, def, parse_args};
use crate::{
    process::{ProcessSpec, run_process},
    sandbox::{self, SandboxConfig, SandboxMode},
    tool::{Risk, Tool, ToolCtx, ToolOutcome},
    workspace::Access,
};

const MAX_TIMEOUT_SECS: u64 = 600;
/// Output kept in memory per command. Anything beyond it is lost, and the marker says so.
const CAPTURE_LIMIT: usize = 8 * 1024 * 1024;
/// Environment variables passed to commands, besides the ones named with `--env`.
const ENV_ALLOW: &[&str] = &[
    "PATH", "HOME", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "TMPDIR",
];
const ENV_ALLOW_PREFIXES: &[&str] = &["CARGO_", "RUSTUP_"];

static OUTPUT_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct RunCommand {
    pub sandbox: SandboxConfig,
    /// Environment variable names to pass through in addition to the allowlist (`--env`).
    pub env_pass: Vec<String>,
    /// Refuse to run without the sandbox (set in `full` mode unless `--sandbox none` was given).
    pub require_sandbox: bool,
    /// Time limit when the model does not ask for one (`--command-timeout`).
    pub default_timeout_secs: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    command: String,
    timeout_seconds: Option<u64>,
    cwd: Option<String>,
}

/// The environment a command sees: the allowlist and `--env` names, nothing else.
pub fn command_env(env_pass: &[String]) -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| {
            ENV_ALLOW.contains(&k.as_str())
                || ENV_ALLOW_PREFIXES.iter().any(|p| k.starts_with(p))
                || env_pass.iter().any(|n| n == k)
        })
        .collect()
}

/// Keeps the first and last `max / 2` bytes of `text`, with a marker in between.
fn head_and_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let half = max / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let omitted = tail_start - head_end;
    format!(
        "{}\n[... {omitted} bytes omitted ...]\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn def(&self) -> ToolDef {
        def(
            "run_command",
            "Run a shell command in the project folder and return its exit code and output. Use it for builds, tests and checks.",
            json!({
                "command": {"type": "string", "description": "The shell command, run with bash -c."},
                "timeout_seconds": {"type": "integer", "description": "Time limit, up to 600. Default 120."},
                "cwd": {"type": "string", "description": "Folder to run in, relative to the project root. Default: the root."}
            }),
            &["command"],
        )
    }

    fn risk(&self, _args: &Map<String, Value>, _ctx: &ToolCtx) -> Risk {
        Risk::Execute
    }

    fn command_text(&self, args: &Map<String, Value>) -> Option<String> {
        args.get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn summary(&self, args: &Map<String, Value>, _ctx: &ToolCtx) -> String {
        let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
        format!("run: {cmd}")
    }

    fn run(&self, args: &Map<String, Value>, ctx: &ToolCtx) -> ToolOutcome {
        let a: RunArgs = match parse_args("run_command", args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let cwd_shown = a.cwd.clone().unwrap_or_default();
        let cwd: PathBuf = match ctx.workspace.resolve(
            if cwd_shown.is_empty() {
                "."
            } else {
                &cwd_shown
            },
            Access::Read,
        ) {
            Ok(p) if p.is_dir() => p,
            Ok(_) => return ToolOutcome::err(format!("'{cwd_shown}' is not a folder")),
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        let timeout_secs = a
            .timeout_seconds
            .unwrap_or(self.default_timeout_secs)
            .clamp(1, MAX_TIMEOUT_SECS);

        let inner = vec![
            "bash".to_string(),
            "-c".to_string(),
            format!("exec 2>&1\n{}", a.command),
        ];
        let (argv, sandboxed) = match (&self.sandbox.bwrap, self.sandbox.mode) {
            (Some(bwrap), SandboxMode::Auto | SandboxMode::Bwrap) => {
                let masks = ctx.workspace.secret_files();
                (
                    sandbox::wrap(
                        &self.sandbox,
                        bwrap,
                        ctx.workspace.root(),
                        &cwd,
                        &masks,
                        &inner,
                    ),
                    true,
                )
            }
            (None, SandboxMode::Bwrap) => {
                return ToolOutcome::err(
                    "bwrap is required by --sandbox bwrap but is not available here",
                );
            }
            _ => (inner, false),
        };
        if !sandboxed && self.require_sandbox {
            return ToolOutcome::err(
                "commands need the bwrap sandbox in full mode, and it is not available. Install bubblewrap, or start with --sandbox none if the environment is trusted.",
            );
        }

        let env = command_env(&self.env_pass);
        let spec = ProcessSpec {
            argv: &argv,
            stdin: b"",
            cwd: Some(&cwd),
            env: Some(env),
            timeout: Duration::from_secs(timeout_secs),
            limit: CAPTURE_LIMIT,
            cancel: Some(&ctx.cancel),
        };
        let result = match run_process(&spec) {
            Ok(r) => r,
            Err(e) => return ToolOutcome::err(format!("cannot start the command: {e}")),
        };

        let output = String::from_utf8_lossy(&result.stdout)
            .trim_end()
            .to_string();
        let status = if result.timed_out {
            format!(
                "timed out after {timeout_secs}s; the command and everything it started were killed"
            )
        } else {
            match (result.code, result.signal) {
                (Some(code), _) => format!("exit code {code}"),
                (None, Some(sig)) => format!("killed by signal {sig}"),
                (None, None) => "no exit status".to_string(),
            }
        };
        let status = if result.cancelled {
            "cancelled by the user; the command and everything it started were killed".to_string()
        } else {
            status
        };
        let mut out = format!("{status}\n");
        let big = output.len() > MAX_OUTPUT_BYTES || result.truncated;
        if big {
            let saved = save_full_output(ctx, &output);
            out.push_str(&head_and_tail(&output, MAX_OUTPUT_BYTES));
            match saved {
                Some(shown) => out.push_str(&format!(
                    "\n[output truncated; the full output is in {shown}, which read_file can read]"
                )),
                None => out.push_str("\n[output truncated]"),
            }
        } else {
            out.push_str(&output);
        }
        if result.orphans_killed {
            out.push_str("\n[background processes started by the command were killed]");
        }
        if result.pipes_abandoned {
            out.push_str("\n[some output was lost: the output pipes stayed open]");
        }
        if !sandboxed {
            out.push_str("\n[note: this command ran without the sandbox]");
        }
        ToolOutcome {
            text: out,
            is_error: !result.success(),
            truncated: big,
        }
    }
}

/// Writes the whole output to the session's scratch folder. Returns the path relative to the
/// workspace, or `None` if there is no scratch folder or the write failed.
fn save_full_output(ctx: &ToolCtx, output: &str) -> Option<String> {
    let dir = ctx.scratch.as_ref()?;
    fs::create_dir_all(dir).ok()?;
    let n = OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let path = dir.join(format!("command-{n}.txt"));
    fs::write(&path, output).ok()?;
    Some(ctx.workspace.relative(&path))
}
