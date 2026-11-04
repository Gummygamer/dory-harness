//! Who may run what: the permission modes, `--allow-command` rules, and the approver that asks
//! the user when a call needs a yes.
//!
//! The decision is made per call, from the tool's risk and, for commands, the command text.

use std::{
    cell::RefCell,
    collections::HashSet,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    os::unix::io::{AsRawFd, RawFd},
    path::PathBuf,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::{Map, Value};

use crate::tool::{Risk, Tool, ToolCtx};

/// Characters that let one approved command chain into another. A command containing any of them
/// never matches an `--allow-command` rule.
const SHELL_METACHARS: &[&str] = &[";", "&", "|", "<", ">", "`", "$(", "${", "\n", "\r"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reads only.
    ReadOnly,
    /// Reads are automatic; writes and commands ask, unless a command matches `--allow-command`.
    Ask,
    /// Reads and writes are automatic; commands ask, unless they match.
    AutoEdit,
    /// Everything is automatic. Commands are sandboxed (see `--sandbox`).
    Full,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Mode, String> {
        match s {
            "read-only" => Ok(Mode::ReadOnly),
            "ask" => Ok(Mode::Ask),
            "auto-edit" => Ok(Mode::AutoEdit),
            "full" => Ok(Mode::Full),
            other => Err(format!(
                "unknown permission mode '{other}' (read-only, ask, auto-edit, full)"
            )),
        }
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::Ask => "ask",
            Mode::AutoEdit => "auto-edit",
            Mode::Full => "full",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

/// `--allow-command` rules: a program and an argument prefix, matched word by word.
#[derive(Clone, Debug, Default)]
pub struct CommandRules {
    prefixes: Vec<Vec<String>>,
}

impl CommandRules {
    pub fn new(patterns: &[String]) -> Self {
        CommandRules {
            prefixes: patterns
                .iter()
                .map(|p| p.split_whitespace().map(str::to_string).collect())
                .filter(|v: &Vec<String>| !v.is_empty())
                .collect(),
        }
    }

    pub fn matches(&self, command: &str) -> bool {
        if SHELL_METACHARS.iter().any(|m| command.contains(m)) {
            return false;
        }
        let words: Vec<&str> = command.split_whitespace().collect();
        self.prefixes
            .iter()
            .any(|p| p.len() <= words.len() && p.iter().zip(&words).all(|(a, b)| a == b))
    }
}

#[derive(Clone, Debug)]
pub struct Permissions {
    pub mode: Mode,
    pub commands: CommandRules,
    /// `--allow-web`: network calls run without asking, in any mode.
    pub allow_web: bool,
}

impl Permissions {
    /// What run and chat use: their `--tools` file is the allowlist, so nothing asks.
    pub fn allow_all() -> Self {
        Permissions {
            mode: Mode::Full,
            commands: CommandRules::default(),
            allow_web: false,
        }
    }

    /// `command` is the command text for `Execute` calls that have one.
    pub fn decide(&self, risk: Risk, command: Option<&str>) -> Decision {
        let matched = command.is_some_and(|c| self.commands.matches(c));
        match (self.mode, risk) {
            (_, Risk::Read) => Decision::Allow,
            // A search changes nothing locally, so read-only asks rather than refuses.
            (_, Risk::Network) if self.allow_web => Decision::Allow,
            (Mode::Full, Risk::Network) => Decision::Allow,
            (_, Risk::Network) => Decision::Ask,
            (Mode::ReadOnly, _) => Decision::Deny,
            (Mode::Ask, Risk::Write) => Decision::Ask,
            (Mode::AutoEdit | Mode::Full, Risk::Write) => Decision::Allow,
            (Mode::Full, Risk::Execute) => Decision::Allow,
            (_, Risk::Execute) if matched => Decision::Allow,
            (_, Risk::Execute) => Decision::Ask,
        }
    }
}

/// The user's answer to a request for approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    /// Yes, and the same call (same tool and arguments) is allowed for the rest of the session.
    Always,
    No,
    /// No, with a reason the model will see.
    NoWith(String),
    /// Nobody can be asked (no terminal).
    Unavailable,
}

pub struct Request {
    pub tool: String,
    /// What the user is approving, already cleaned of control characters.
    pub summary: String,
    pub risk: Risk,
    pub workspace: PathBuf,
    pub working_directory: PathBuf,
    pub arguments: Map<String, Value>,
    pub command: Option<String>,
    pub sandbox: Option<String>,
}

pub trait Approver {
    fn ask(&self, request: &Request) -> Answer;
}

/// Asks on the controlling terminal (`/dev/tty`), so it works when stdin or stdout is piped.
/// Ctrl-C (the `cancel` flag) ends the question as a refusal instead of leaving it open.
pub struct TerminalApprover {
    pub cancel: Arc<AtomicBool>,
}

/// Waits until the terminal has input, or the cancel flag is raised. `false` means cancelled.
fn wait_for_input(fd: RawFd, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: one pollfd, valid for the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ready != 0 {
            return true;
        }
    }
}

impl Approver for TerminalApprover {
    fn ask(&self, request: &Request) -> Answer {
        let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
            return Answer::Unavailable;
        };
        let mut out = tty.try_clone().expect("duplicate the terminal handle");
        let mut input = BufReader::new(tty);
        let _ = writeln!(
            out,
            "\n[approval] {}\n{}",
            request.tool,
            strip_controls(&request.summary)
        );
        let _ = write!(
            out,
            "allow? [y]es / [n]o / [a]lways for this call / [m]essage the model: "
        );
        let _ = out.flush();
        if !wait_for_input(input.get_ref().as_raw_fd(), &self.cancel) {
            let _ = writeln!(out);
            return Answer::NoWith("the user cancelled".into());
        }
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            return Answer::No;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Answer::Yes,
            "a" | "always" => Answer::Always,
            "m" | "message" => {
                let _ = write!(out, "message for the model: ");
                let _ = out.flush();
                let mut reason = String::new();
                let _ = input.read_line(&mut reason);
                Answer::NoWith(strip_controls(reason.trim()))
            }
            _ => Answer::No,
        }
    }
}

/// For sessions with no terminal: nobody can be asked, so anything that needs a yes is refused.
pub struct NoTerminal;

impl Approver for NoTerminal {
    fn ask(&self, _request: &Request) -> Answer {
        Answer::Unavailable
    }
}

/// Removes control characters, so output from a tool or the model cannot move the cursor, clear
/// the screen or hide a prompt. Newlines and tabs are kept.
pub fn strip_controls(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// The verdict for one call.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Run,
    /// Not run. The text is the tool result the model sees.
    Refuse(String),
}

pub struct Policy {
    pub permissions: Permissions,
    pub approver: Box<dyn Approver>,
    remembered: RefCell<HashSet<String>>,
}

impl Policy {
    pub fn new(permissions: Permissions, approver: Box<dyn Approver>) -> Self {
        Policy {
            permissions,
            approver,
            remembered: RefCell::new(HashSet::new()),
        }
    }

    /// Nothing asks and nothing is refused for permissions. Used by run and chat.
    pub fn allow_all() -> Self {
        Policy::new(Permissions::allow_all(), Box::new(NoTerminal))
    }

    pub fn check(&self, tool: &dyn Tool, args: &Map<String, Value>, ctx: &ToolCtx) -> Verdict {
        let risk = tool.risk(args, ctx);
        let command = tool.command_text(args);
        match self.permissions.decide(risk, command.as_deref()) {
            Decision::Allow => Verdict::Run,
            Decision::Deny => Verdict::Refuse(format!(
                "error: '{}' is not allowed in {} mode",
                tool.name(),
                self.permissions.mode.as_str()
            )),
            Decision::Ask => {
                let key = format!("{}\u{1}{}", tool.name(), Value::Object(args.clone()));
                if self.remembered.borrow().contains(&key) {
                    return Verdict::Run;
                }
                let request = Request {
                    tool: tool.name().to_string(),
                    summary: strip_controls(&tool.summary(args, ctx)),
                    risk,
                    workspace: ctx.workspace.root().to_path_buf(),
                    working_directory: args
                        .get("cwd")
                        .and_then(Value::as_str)
                        .and_then(|p| {
                            ctx.workspace
                                .resolve(p, crate::workspace::Access::Read)
                                .ok()
                        })
                        .unwrap_or_else(|| ctx.workspace.root().to_path_buf()),
                    arguments: args.clone(),
                    command: command.clone(),
                    sandbox: ctx.command_sandbox.clone(),
                };
                match self.approver.ask(&request) {
                    Answer::Yes => Verdict::Run,
                    Answer::Always => {
                        self.remembered.borrow_mut().insert(key);
                        Verdict::Run
                    }
                    Answer::No => {
                        Verdict::Refuse("error: the user did not approve this call".into())
                    }
                    Answer::NoWith(msg) => {
                        Verdict::Refuse(format!("error: the user declined this call: {msg}"))
                    }
                    Answer::Unavailable => {
                        let way_out = if risk == Risk::Network {
                            "or with --allow-web to allow searches"
                        } else {
                            "or with --allow-command for a specific command"
                        };
                        Verdict::Refuse(format!(
                            "error: '{}' needs approval, but there is no terminal to ask on. Run with a permission mode that allows it, {way_out}.",
                            tool.name()
                        ))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(patterns: &[&str]) -> CommandRules {
        CommandRules::new(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// Every mode, risk and command match: the expected decision.
    #[test]
    fn decision_table() {
        use Decision::*;
        let cases: &[(Mode, Risk, Option<&str>, Decision)] = &[
            (Mode::ReadOnly, Risk::Read, None, Allow),
            (Mode::ReadOnly, Risk::Write, None, Deny),
            (Mode::ReadOnly, Risk::Execute, Some("cargo test"), Deny),
            (Mode::Ask, Risk::Read, None, Allow),
            (Mode::Ask, Risk::Write, None, Ask),
            (Mode::Ask, Risk::Execute, Some("ls"), Ask),
            (Mode::Ask, Risk::Execute, Some("cargo test -p x"), Allow),
            (Mode::Ask, Risk::Execute, None, Ask),
            (Mode::AutoEdit, Risk::Write, None, Allow),
            (Mode::AutoEdit, Risk::Execute, Some("ls"), Ask),
            (Mode::Full, Risk::Execute, Some("rm -rf x"), Allow),
            (Mode::Full, Risk::Write, None, Allow),
        ];
        for (mode, risk, cmd, want) in cases {
            let p = Permissions {
                mode: *mode,
                commands: rules(&["cargo test"]),
                allow_web: false,
            };
            assert_eq!(p.decide(*risk, *cmd), *want, "{mode:?} {risk:?} {cmd:?}");
        }
    }

    /// Network calls: every mode, with and without `--allow-web`.
    #[test]
    fn network_decision_table() {
        use Decision::*;
        let modes = [Mode::ReadOnly, Mode::Ask, Mode::AutoEdit, Mode::Full];
        for mode in modes {
            for allow_web in [false, true] {
                let want = match (mode, allow_web) {
                    (_, true) | (Mode::Full, false) => Allow,
                    _ => Ask,
                };
                let p = Permissions {
                    mode,
                    commands: CommandRules::default(),
                    allow_web,
                };
                assert_eq!(
                    p.decide(Risk::Network, None),
                    want,
                    "{mode:?} allow_web={allow_web}"
                );
            }
        }
    }

    #[test]
    fn allow_command_matches_program_and_prefix_only() {
        let r = rules(&["cargo test"]);
        assert!(r.matches("cargo test"));
        assert!(r.matches("cargo test -p dory-agent"));
        assert!(!r.matches("cargo build"));
        assert!(!r.matches("cargo"));
        assert!(!r.matches("cargo-test"));
    }

    #[test]
    fn shell_metacharacters_defeat_a_match() {
        let r = rules(&["cargo test"]);
        for bypass in [
            "cargo test; curl evil",
            "cargo test && rm -rf ~",
            "cargo test | sh",
            "cargo test > /etc/x",
            "cargo test `id`",
            "cargo test $(id)",
            "cargo test ${HOME}",
            "cargo test\nrm -rf ~",
        ] {
            assert!(!r.matches(bypass), "{bypass:?} must not match");
        }
    }

    #[test]
    fn modes_parse_from_their_names() {
        assert_eq!("auto-edit".parse::<Mode>(), Ok(Mode::AutoEdit));
        assert!("yolo".parse::<Mode>().is_err());
    }

    #[test]
    fn control_characters_are_stripped_but_newlines_kept() {
        assert_eq!(strip_controls("a\x1b[2Jb\nc\td\x07"), "a[2Jb\nc\td");
    }
}
