//! Runs commands under `bwrap` (bubblewrap): read-only root, the workspace writable, a private
//! `/tmp`, no network unless asked for, and the home folder's keys hidden behind empty mounts.
//!
//! Path checks only confine the file tools. A shell command can reach whatever the user can, so
//! the sandbox is what limits commands.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
};

/// Folders under `$HOME` that hold keys or credentials. Each is replaced by an empty mount.
const HIDDEN_HOME_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    /// Use bwrap when it works. Otherwise run without a sandbox, with a warning.
    Auto,
    /// Use bwrap; refuse to run commands without it.
    Bwrap,
    /// Never sandbox commands. Only for a trusted environment.
    None,
}

impl FromStr for SandboxMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(SandboxMode::Auto),
            "bwrap" => Ok(SandboxMode::Bwrap),
            "none" => Ok(SandboxMode::None),
            other => Err(format!("unknown sandbox mode '{other}' (auto, bwrap, none)")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SandboxConfig {
    pub mode: SandboxMode,
    /// Allow network access inside the sandbox.
    pub network: bool,
    /// Extra directories the commands may write to, besides the workspace (`--writable`).
    pub writable: Vec<PathBuf>,
    /// The home folder whose key directories are hidden. `None` hides nothing.
    pub home: Option<PathBuf>,
    /// The `bwrap` binary, found and tested at startup. `None` if unavailable.
    pub bwrap: Option<PathBuf>,
}

impl SandboxConfig {
    pub fn new(mode: SandboxMode) -> Self {
        SandboxConfig {
            mode,
            network: false,
            writable: Vec::new(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            bwrap: if mode == SandboxMode::None { None } else { probe_bwrap() },
        }
    }

    /// Whether commands will run under bwrap.
    pub fn active(&self) -> bool {
        self.mode != SandboxMode::None && self.bwrap.is_some()
    }
}

/// Finds `bwrap` on PATH and checks that it can create its namespaces here. Some systems block
/// unprivileged user namespaces, so the binary's presence is not enough.
pub fn probe_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let bwrap = std::env::split_paths(&path).map(|d| d.join("bwrap")).find(|p| p.is_file())?;
    let ok = Command::new(&bwrap)
        .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/", "/", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(bwrap)
}

/// The full argv for running `inner` (a program and its arguments) inside the sandbox.
pub fn wrap(cfg: &SandboxConfig, bwrap: &Path, workspace: &Path, cwd: &Path, inner: &[String]) -> Vec<String> {
    let mut a: Vec<String> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut bind = |dir: &Path| {
        let d = dir.display().to_string();
        a.extend(["--bind".to_string(), d.clone(), d]);
    };
    bind(workspace);
    for dir in &cfg.writable {
        bind(dir);
    }
    if let Some(home) = &cfg.home {
        for sub in HIDDEN_HOME_DIRS {
            let p = home.join(sub);
            if p.exists() {
                a.extend(["--tmpfs".to_string(), p.display().to_string()]);
            }
        }
    }
    if cfg.network {
        a.push("--share-net".into());
    }
    a.extend(["--chdir".to_string(), cwd.display().to_string()]);
    let mut argv = vec![bwrap.display().to_string()];
    argv.extend(a);
    argv.push("--".into());
    argv.extend(inner.iter().cloned());
    argv
}
