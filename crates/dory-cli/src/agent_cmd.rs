//! `dory agent`: the agent loop from the command line.
//!
//! Output: the answer on stdout and everything else on stderr, or with `--json` one JSON event per
//! line on stdout (schema version 1, see docs/AGENT_TOOLS_PLAN.md). Exit codes: 0 answered,
//! 1 runtime error or the model could not finish, 3 a limit was reached (rounds, time or context),
//! 4 cancelled.

use std::{
    fs,
    io::{BufRead, Write, stderr, stdin, stdout},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use clap::Args;
use dory_agent::{
    Agent, AgentConfig, AgentEvent, Reason, Registry, TurnReport,
    backend::EngineBackend,
    builtin::{command::RunCommand, read_tools, write_tools},
    policy::{CommandRules, Mode, Permissions, Policy, TerminalApprover},
    prompt,
    sandbox::{SandboxConfig, SandboxMode},
    session::Session,
    tool::ToolCtx,
    tools::ToolSet,
    workspace::Workspace,
};
use dory_engine::{CacheMode, Engine, chat::Message, verify};
use serde_json::{Value, json};

use crate::{Cli, gen_params, print_event, start_engine};

/// The version of the `--json` event schema.
const JSON_VERSION: u32 = 1;
/// Ctrl-C twice within this window exits at once.
const DOUBLE_PRESS: Duration = Duration::from_secs(2);

#[derive(Args, Clone)]
pub struct AgentArgs {
    /// The task. Without it, an interactive session starts.
    prompt: Option<String>,
    /// The project folder the tools may read and change.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// read-only, ask, auto-edit or full.
    #[arg(long, default_value = "ask")]
    permission: String,
    /// A command that may run without asking: a program and an argument prefix. Repeatable.
    #[arg(long = "allow-command", value_name = "PATTERN")]
    allow_commands: Vec<String>,
    /// A path (glob) the file tools may read and write even though it looks like a secret. Repeatable.
    #[arg(long = "allow-path", value_name = "GLOB")]
    allow_paths: Vec<String>,
    /// auto (bwrap when it works), bwrap, or none.
    #[arg(long, default_value = "auto")]
    sandbox: String,
    /// Allow network access inside the sandbox.
    #[arg(long)]
    network: bool,
    /// A folder commands may write to besides the project. Repeatable.
    #[arg(long = "writable", value_name = "DIR")]
    writable: Vec<PathBuf>,
    /// An environment variable to pass to commands, besides the defaults. Repeatable.
    #[arg(long = "env", value_name = "NAME")]
    env_pass: Vec<String>,
    /// Model rounds per task, not counting the final answer round.
    #[arg(long, default_value_t = 30)]
    max_rounds: usize,
    /// Wall-clock limit for one task, checked between rounds.
    #[arg(long, default_value_t = 1800)]
    max_seconds: u64,
    /// Time limit for a command when the model does not ask for one (up to 600 s).
    #[arg(long, default_value_t = 120)]
    command_timeout: u64,
    /// Write JSON events to stdout, one per line.
    #[arg(long)]
    json: bool,
    /// Continue a session by its id (see .dory/sessions in the project).
    #[arg(long)]
    resume: Option<String>,
    /// Extra instructions added to the system prompt.
    #[arg(long)]
    system: Option<String>,
    /// Show full tool output on stderr instead of the first 200 characters.
    #[arg(long)]
    verbose: bool,
}

/// Ctrl-C state shared with the signal handler.
struct Interrupt {
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    last_press: Arc<Mutex<Option<Instant>>>,
}

fn install_interrupt(state: &Interrupt) -> Result<()> {
    let cancel = state.cancel.clone();
    let busy = state.busy.clone();
    let last = state.last_press.clone();
    ctrlc::set_handler(move || {
        if !busy.load(Ordering::SeqCst) {
            // Nothing is running: leave now.
            eprintln!();
            std::process::exit(130);
        }
        let now = Instant::now();
        let mut last = last.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_some_and(|t| now.duration_since(t) < DOUBLE_PRESS) {
            std::process::exit(4);
        }
        *last = Some(now);
        cancel.store(true, Ordering::SeqCst);
        eprintln!("\n[cancelling; press Ctrl-C again within 2 s to exit]");
    })
    .context("installing the Ctrl-C handler")
}

/// Turns events into output: JSON lines on stdout, or the human form on stdout and stderr.
struct Output {
    json: bool,
    show_reasoning: bool,
    max_rounds: usize,
    verbose: bool,
}

impl Output {
    fn event(&self, ev: &AgentEvent) {
        if self.json {
            if let Some(v) = json_event(ev) {
                let _ = writeln!(stdout(), "{v}");
                let _ = stdout().flush();
            }
        } else {
            print_event(ev.clone(), self.show_reasoning, self.max_rounds, self.verbose);
        }
    }

    fn line(&self, kind: &str, v: Value) {
        if self.json {
            let mut obj = v;
            if let Some(o) = obj.as_object_mut() {
                o.insert("version".into(), json!(JSON_VERSION));
                o.insert("event".into(), json!(kind));
            }
            let _ = writeln!(stdout(), "{obj}");
            let _ = stdout().flush();
        } else {
            let _ = writeln!(stderr(), "{v}");
        }
    }
}

fn json_event(ev: &AgentEvent) -> Option<Value> {
    let v = JSON_VERSION;
    Some(match ev {
        AgentEvent::Reasoning(t) => json!({"version": v, "event": "reasoning", "text": t}),
        AgentEvent::Content(t) => json!({"version": v, "event": "content", "text": t}),
        AgentEvent::ToolCall { name, arguments } => {
            json!({"version": v, "event": "tool_call", "name": name, "arguments": arguments})
        }
        AgentEvent::MalformedCall(raw) => json!({"version": v, "event": "malformed_call", "raw": raw}),
        AgentEvent::ToolResult { name, text } => json!({"version": v, "event": "tool_result", "name": name, "text": text}),
        AgentEvent::Compacted { stage, before, after } => {
            json!({"version": v, "event": "compacted", "stage": stage, "before": before, "after": after})
        }
        AgentEvent::RoundStats(_) | AgentEvent::MessageAdded(_) | AgentEvent::Finished(_) => return None,
    })
}

fn exit_code(reason: Option<Reason>) -> i32 {
    match reason {
        Some(Reason::Answered) => 0,
        Some(Reason::Cancelled) => 4,
        Some(Reason::RoundLimit | Reason::TimeLimit | Reason::ContextFull) => 3,
        _ => 1,
    }
}

/// Everything one turn needs, built once per command.
struct Setup<'a> {
    engine: &'a Engine,
    registry: Registry,
    policy: Policy,
    thinking: bool,
    params: dory_engine::GenParams,
    max_rounds: usize,
    max_duration: Option<Duration>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    last_press: Arc<Mutex<Option<Instant>>>,
}

pub fn run(cli: &Cli, args: &AgentArgs) -> Result<i32> {
    let mode: Mode = args.permission.parse().map_err(anyhow::Error::msg)?;
    let sandbox_mode: SandboxMode = args.sandbox.parse().map_err(anyhow::Error::msg)?;
    let root = fs::canonicalize(&args.workspace).with_context(|| format!("workspace {}", args.workspace.display()))?;
    let workspace = Workspace::new(&root, &args.allow_paths)?;

    let mut sandbox = SandboxConfig::new(sandbox_mode);
    sandbox.network = args.network;
    for dir in &args.writable {
        sandbox.writable.push(fs::canonicalize(dir).with_context(|| format!("--writable {}", dir.display()))?);
    }
    let require_sandbox = mode == Mode::Full && sandbox_mode != SandboxMode::None;

    let cancel = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicBool::new(false));
    let last_press = Arc::new(Mutex::new(None));
    install_interrupt(&Interrupt { cancel: cancel.clone(), busy: busy.clone(), last_press: last_press.clone() })?;

    let engine = start_engine(cli)?;

    // The session comes first: its scratch folder is where long command output goes.
    let header = json!({
        "model": cli.load.model_path()?.display().to_string(),
        "loops": cli.load.loops,
        "cache": CacheMode::from(cli.load.cache_mode).as_str(),
        "schedule": format!("{:?}", cli.gen_args.prefill),
        "seed": cli.gen_args.seed,
        "commit": verify::PINNED_FORK_COMMIT,
        "workspace": root.display().to_string(),
        "permission": mode.as_str(),
        "sandbox": if sandbox.active() { "bwrap" } else { "none" },
    });
    let (mut session, mut messages) = match &args.resume {
        Some(id) => {
            let (s, msgs, _) = Session::open(&root, id).with_context(|| format!("resuming session {id}"))?;
            (s, msgs)
        }
        None => {
            let mut s = Session::create(&root, header).context("creating the session transcript")?;
            let agents_md = fs::read_to_string(root.join("AGENTS.md")).ok();
            let system = prompt::system_prompt(&root, std::env::consts::OS, agents_md.as_deref(), args.system.as_deref());
            let msgs = vec![Message::new("system", &system)];
            s.record(&AgentEvent::MessageAdded(msgs[0].clone()))?;
            (s, msgs)
        }
    };

    let mut ctx = ToolCtx::with_cancel(workspace, cancel.clone());
    ctx.scratch = Some(session.scratch_dir());
    let mut registry = Registry::new(ctx);
    for t in read_tools().into_iter().chain(write_tools()) {
        registry.add(t)?;
    }
    registry.add(Box::new(RunCommand {
        sandbox: sandbox.clone(),
        env_pass: args.env_pass.clone(),
        require_sandbox,
        default_timeout_secs: args.command_timeout,
    }))?;
    if let Some(p) = &cli.gen_args.tools {
        ToolSet::load(p)?.register_into(&mut registry)?;
    }

    let policy = Policy::new(
        Permissions { mode, commands: CommandRules::new(&args.allow_commands) },
        Box::new(TerminalApprover { cancel: cancel.clone() }),
    );

    let sandbox_state = if sandbox.active() {
        "bwrap".to_string()
    } else if sandbox_mode == SandboxMode::None {
        "none (--sandbox none)".to_string()
    } else {
        "none (bwrap is not available here)".to_string()
    };
    eprintln!(
        "agent: workspace {} | permission {} | sandbox {} | network {} | session {}",
        root.display(),
        mode.as_str(),
        sandbox_state,
        if args.network { "on" } else { "off" },
        session.id
    );
    if mode == Mode::Full {
        eprintln!("warning: full mode runs commands without asking.");
        if !sandbox.active() {
            eprintln!("warning: commands are NOT sandboxed. Use full mode only on trusted input.");
        }
    }

    let setup = Setup {
        engine: &engine,
        registry,
        policy,
        thinking: !cli.gen_args.no_think,
        params: gen_params(&cli.gen_args),
        max_rounds: args.max_rounds,
        max_duration: Some(Duration::from_secs(args.max_seconds)),
        cancel,
        busy,
        last_press,
    };
    let output = Output {
        json: args.json,
        show_reasoning: !cli.gen_args.quiet_reasoning,
        max_rounds: args.max_rounds,
        verbose: args.verbose,
    };
    if args.json {
        output.line("session", json!({"id": session.id}));
    }

    let code = match &args.prompt {
        Some(task) => {
            messages.push(Message::new("user", task));
            session.record(&AgentEvent::MessageAdded(messages.last().unwrap().clone()))?;
            let (reason, totals) = run_turn(&setup, &mut session, &output, &mut messages)?;
            finish_line(&output, &session, reason, &totals);
            exit_code(reason)
        }
        None => repl(&setup, &mut session, &output, &mut messages, &root)?,
    };
    Ok(code)
}

#[derive(Default)]
struct Totals {
    rounds: usize,
    tool_calls: usize,
    prompt_tokens: usize,
    generated: usize,
}

fn finish_line(output: &Output, session: &Session, reason: Option<Reason>, totals: &Totals) {
    let reason = reason.map(Reason::as_str).unwrap_or("none");
    if output.json {
        output.line(
            "finished",
            json!({
                "reason": reason,
                "rounds": totals.rounds,
                "tool_calls": totals.tool_calls,
                "prompt_tokens": totals.prompt_tokens,
                "generated_tokens": totals.generated,
                "session": session.id,
            }),
        );
    } else {
        eprintln!(
            "[{reason}] rounds {}, tool calls {}, prompt {} tokens, generated {} tokens, session {}",
            totals.rounds, totals.tool_calls, totals.prompt_tokens, totals.generated, session.id
        );
    }
}

/// Runs one task to its end. Each message and round is written to the transcript as it happens.
fn run_turn(setup: &Setup, session: &mut Session, output: &Output, messages: &mut Vec<Message>) -> Result<(Option<Reason>, Totals)> {
    setup.cancel.store(false, Ordering::SeqCst);
    *setup.last_press.lock().unwrap_or_else(|p| p.into_inner()) = None;
    setup.busy.store(true, Ordering::SeqCst);

    let backend = EngineBackend { engine: setup.engine };
    let mut config = AgentConfig::new(setup.thinking, setup.params.clone());
    config.max_rounds = setup.max_rounds;
    config.max_duration = setup.max_duration;
    let agent = Agent {
        backend: &backend,
        tools: &setup.registry,
        policy: &setup.policy,
        config,
        cancel: setup.cancel.clone(),
    };

    let mut reason = None;
    let mut totals = Totals::default();
    let result = {
        let mut sink = |ev: AgentEvent| {
            match &ev {
                AgentEvent::Finished(r) => reason = Some(*r),
                AgentEvent::RoundStats(s) => {
                    totals.rounds += 1;
                    totals.prompt_tokens += s.prompt_tokens;
                    totals.generated += s.generated;
                }
                AgentEvent::ToolResult { .. } => totals.tool_calls += 1,
                _ => {}
            }
            if let Err(e) = session.record(&ev) {
                eprintln!("warning: could not write the transcript: {e}");
            }
            output.event(&ev);
        };
        agent.run_turn(messages, &mut sink)
    };
    setup.busy.store(false, Ordering::SeqCst);
    let _: TurnReport = result?;
    Ok((reason, totals))
}

/// The interactive session. Commands: /exit, /reset, /permission MODE, /compact, /tools, /status.
fn repl(setup: &Setup, session: &mut Session, output: &Output, messages: &mut Vec<Message>, root: &std::path::Path) -> Result<i32> {
    let system: Vec<Message> = messages.iter().take(1).cloned().collect();
    eprintln!("ready. /exit, /reset, /permission MODE, /compact, /tools, /status");
    let mut policy_mode = setup.policy.permissions.mode;
    let mut lines = stdin().lock().lines();
    loop {
        eprint!("you> ");
        let _ = stderr().flush();
        let Some(line) = lines.next() else { break };
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "/exit" => break,
            "/reset" => {
                *messages = system.clone();
                eprintln!("[conversation cleared; the transcript is kept]");
                continue;
            }
            "/compact" => {
                let agent_backend = EngineBackend { engine: setup.engine };
                let agent = Agent {
                    backend: &agent_backend,
                    tools: &setup.registry,
                    policy: &setup.policy,
                    config: AgentConfig::new(setup.thinking, setup.params.clone()),
                    cancel: setup.cancel.clone(),
                };
                let changed = agent.compact_now(messages, &mut |ev| output.event(&ev))?;
                eprintln!("[{}]", if changed { "compacted" } else { "nothing to compact" });
                continue;
            }
            "/tools" => {
                let names: Vec<String> = setup.registry.defs().into_iter().map(|d| d.name).collect();
                eprintln!("[tools: {}]", names.join(", "));
                continue;
            }
            "/status" => {
                eprintln!(
                    "[session {} | workspace {} | permission {} | messages {}]",
                    session.id,
                    root.display(),
                    policy_mode.as_str(),
                    messages.len()
                );
                continue;
            }
            _ => {}
        }
        if let Some(arg) = line.strip_prefix("/permission ") {
            match arg.trim().parse::<Mode>() {
                Ok(m) => {
                    policy_mode = m;
                    eprintln!("[permission {}]", m.as_str());
                }
                Err(e) => eprintln!("[{e}]"),
            }
            continue;
        }
        if line.starts_with('/') {
            eprintln!("[unknown command {line}]");
            continue;
        }

        messages.push(Message::new("user", line));
        session.record(&AgentEvent::MessageAdded(messages.last().unwrap().clone()))?;
        print!("dory> ");
        let _ = stdout().flush();
        let (reason, totals) = run_turn(setup, session, output, messages)?;
        println!();
        finish_line(output, session, reason, &totals);
    }
    Ok(0)
}
