//! `run_command`: exit codes, output limits, the sandbox, the environment, timeouts.
//! The sandbox tests need `bwrap` and skip (with a message) when it is not available.

use std::{fs, path::PathBuf};

use dory_agent::{
    Registry, ToolCtx,
    builtin::command::RunCommand,
    sandbox::{SandboxConfig, SandboxMode, probe_bwrap},
    tool::ToolOutcome,
    workspace::Workspace,
};
use serde_json::{Map, Value, json};

/// Fixtures live under the cargo test tmp dir, not /tmp: the sandbox mounts a private /tmp.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("command-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn registry(
    root: &PathBuf,
    sandbox: SandboxConfig,
    require_sandbox: bool,
    scratch_dir: Option<PathBuf>,
) -> Registry {
    let mut ctx = ToolCtx::new(Workspace::new(root, &[]).unwrap());
    ctx.scratch = scratch_dir;
    let mut reg = Registry::new(ctx);
    reg.add(Box::new(RunCommand {
        sandbox,
        env_pass: vec![],
        require_sandbox,
        default_timeout_secs: 120,
    }))
    .unwrap();
    reg
}

fn sandbox_or_skip(home: Option<PathBuf>) -> Option<SandboxConfig> {
    let bwrap = probe_bwrap();
    if bwrap.is_none() {
        eprintln!("skipping: bwrap is not usable here");
        return None;
    }
    let mut cfg = SandboxConfig::new(SandboxMode::Auto);
    cfg.bwrap = bwrap;
    cfg.home = home;
    Some(cfg)
}

fn run(reg: &Registry, command: &str) -> ToolOutcome {
    let args: Map<String, Value> = json!({"command": command}).as_object().unwrap().clone();
    reg.call("run_command", &args)
}

fn run_with(reg: &Registry, v: Value) -> ToolOutcome {
    reg.call("run_command", v.as_object().unwrap())
}

#[test]
fn output_and_exit_code_come_back_in_order() {
    let root = scratch("basic");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        None,
    );
    let out = run(&reg, "echo first; echo second >&2; echo third");
    assert!(!out.is_error);
    assert_eq!(
        out.text,
        "exit code 0\nfirst\nsecond\nthird\n[note: this command ran without the sandbox]"
    );
}

#[test]
fn a_failing_command_reports_its_exit_code_and_is_an_error() {
    let root = scratch("fail");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        None,
    );
    let out = run(&reg, "echo broken; exit 3");
    assert!(out.is_error);
    assert!(out.text.starts_with("exit code 3\nbroken"), "{}", out.text);
}

#[test]
fn the_working_folder_must_be_inside_the_workspace() {
    let root = scratch("cwd");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        None,
    );
    let out = run_with(&reg, json!({"command": "pwd", "cwd": "../.."}));
    assert!(
        out.is_error && out.text.contains("outside the workspace"),
        "{}",
        out.text
    );
}

#[test]
fn a_timeout_kills_the_command_and_what_it_started() {
    let root = scratch("timeout");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        None,
    );
    let start = std::time::Instant::now();
    let out = run_with(
        &reg,
        json!({"command": "sleep 100 & sleep 100", "timeout_seconds": 1}),
    );
    assert!(
        out.is_error && out.text.contains("timed out after 1s"),
        "{}",
        out.text
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(8));
}

#[test]
fn large_output_is_cut_to_head_and_tail_and_the_full_text_is_saved() {
    let root = scratch("big");
    let save = root.join(".dory-scratch");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        Some(save.clone()),
    );
    let out = run(&reg, "head -c 100000 /dev/zero | tr '\\0' a");
    assert!(out.truncated);
    assert!(out.text.len() < 17 * 1024, "kept {} bytes", out.text.len());
    assert!(out.text.contains("bytes omitted"), "{}", out.text);
    assert!(
        out.text.contains(".dory-scratch/command-"),
        "the saved path must be named: {}",
        out.text
    );
    let saved: Vec<_> = fs::read_dir(&save).unwrap().flatten().collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(fs::metadata(saved[0].path()).unwrap().len(), 100_000);
}

#[test]
fn full_mode_refuses_without_a_sandbox_when_required() {
    let root = scratch("require");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        true,
        None,
    );
    let out = run(&reg, "echo should-not-run");
    assert!(
        out.is_error && out.text.contains("need the bwrap sandbox"),
        "{}",
        out.text
    );
}

#[test]
fn commands_cannot_write_outside_the_workspace_under_bwrap() {
    let root = scratch("write");
    let Some(cfg) = sandbox_or_skip(None) else {
        return;
    };
    let reg = registry(&root, cfg, false, None);
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("escape-target");
    let _ = fs::remove_file(&outside);
    // The message text depends on the locale, so the test checks the exit status instead.
    let out = run(
        &reg,
        &format!(
            "if echo x > '{}'; then echo WROTE; else echo REFUSED; fi",
            outside.display()
        ),
    );
    assert!(!outside.exists(), "a command wrote outside the workspace");
    assert!(
        out.text.contains("REFUSED") && !out.text.contains("WROTE"),
        "{}",
        out.text
    );
    let inside = run(&reg, "echo ok > inside.txt && cat inside.txt");
    assert!(inside.text.contains("ok"), "{}", inside.text);
    assert!(root.join("inside.txt").exists());
}

#[test]
fn hidden_key_folders_show_nothing_under_bwrap() {
    let root = scratch("keys");
    let home = scratch("keys-home");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(home.join(".ssh/id_rsa"), "PRIVATE").unwrap();
    let Some(cfg) = sandbox_or_skip(Some(home.clone())) else {
        return;
    };
    let reg = registry(&root, cfg, false, None);
    let out = run(
        &reg,
        &format!("cat '{}/.ssh/id_rsa' 2>&1; echo end", home.display()),
    );
    assert!(
        !out.text.contains("PRIVATE"),
        "the key was readable: {}",
        out.text
    );
}

#[test]
fn secret_files_in_the_workspace_read_as_empty_under_bwrap() {
    let root = scratch("mask");
    fs::write(root.join(".env"), "SECRET=sk-FAKE-mask").unwrap();
    fs::create_dir_all(root.join("svc")).unwrap();
    fs::write(root.join("svc/deploy.pem"), "PEM-FAKE-mask").unwrap();
    fs::write(root.join("open.txt"), "visible").unwrap();
    let Some(cfg) = sandbox_or_skip(None) else {
        return;
    };
    let reg = registry(&root, cfg, false, None);
    let out = run(&reg, "cat .env; cat svc/deploy.pem; cat open.txt; echo end");
    assert!(
        !out.text.contains("sk-FAKE-mask") && !out.text.contains("PEM-FAKE-mask"),
        "{}",
        out.text
    );
    assert!(out.text.contains("visible"), "{}", out.text);
    // The files themselves are untouched on the host.
    assert_eq!(
        fs::read_to_string(root.join(".env")).unwrap(),
        "SECRET=sk-FAKE-mask"
    );
}

#[test]
fn an_allowed_secret_path_is_not_masked() {
    let root = scratch("mask-allow");
    fs::write(root.join(".env"), "SECRET=sk-FAKE-allowed").unwrap();
    let Some(cfg) = sandbox_or_skip(None) else {
        return;
    };
    let mut ctx = ToolCtx::new(Workspace::new(&root, &[".env".to_string()]).unwrap());
    ctx.scratch = None;
    let mut reg = Registry::new(ctx);
    reg.add(Box::new(RunCommand {
        sandbox: cfg,
        env_pass: vec![],
        require_sandbox: false,
        default_timeout_secs: 120,
    }))
    .unwrap();
    let out = run(&reg, "cat .env");
    assert!(out.text.contains("sk-FAKE-allowed"), "{}", out.text);
}

#[test]
fn the_network_is_unreachable_by_default_under_bwrap() {
    let root = scratch("net");
    let Some(cfg) = sandbox_or_skip(None) else {
        return;
    };
    let reg = registry(&root, cfg, false, None);
    let out = run(
        &reg,
        "exec 2>/dev/null; (echo > /dev/tcp/1.1.1.1/80) && echo connected || echo blocked",
    );
    assert!(
        out.text.contains("blocked") && !out.text.contains("connected"),
        "{}",
        out.text
    );
}

#[test]
fn only_allowlisted_environment_variables_reach_the_command() {
    let root = scratch("env");
    let reg = registry(
        &root,
        SandboxConfig {
            mode: SandboxMode::None,
            network: false,
            writable: vec![],
            home: None,
            bwrap: None,
        },
        false,
        None,
    );
    let out = run(&reg, "env");
    let allowed = [
        "PATH", "HOME", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "TMPDIR", "PWD", "SHLVL", "_",
        "OLDPWD",
    ];
    for (key, _) in std::env::vars() {
        let listed = allowed.contains(&key.as_str())
            || key.starts_with("CARGO_")
            || key.starts_with("RUSTUP_");
        if !listed {
            assert!(
                !out.text.contains(&format!("{key}=")),
                "{key} leaked into the command environment"
            );
        }
    }
}
