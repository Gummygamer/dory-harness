//! Write tools: multiple and zero matches, stale files, CRLF endings, permissions, atomic failure.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
};

use dory_agent::{
    Registry, ToolCtx,
    builtin::{read, write},
    tool::ToolOutcome,
    workspace::Workspace,
};
use serde_json::{Map, Value, json};

struct Fixture {
    dir: tempfile::TempDir,
    reg: Registry,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let mut reg = Registry::new(ToolCtx::new(Workspace::new(dir.path(), &[]).unwrap()));
    for t in read::tools().into_iter().chain(write::tools()) {
        reg.add(t).unwrap();
    }
    Fixture { dir, reg }
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn call(f: &Fixture, name: &str, v: Value) -> ToolOutcome {
    f.reg.call(name, &args(v))
}

fn read_back(f: &Fixture, rel: &str) -> String {
    fs::read_to_string(f.dir.path().join(rel)).unwrap()
}

fn seed(f: &Fixture, rel: &str, content: &str) {
    let p = f.dir.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

#[test]
fn write_file_creates_new_files_in_new_folders_without_a_read() {
    let f = fixture();
    let out = call(&f, "write_file", json!({"path": "a/b/c.txt", "content": "hello\n"}));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.starts_with("created a/b/c.txt"), "{}", out.text);
    assert_eq!(read_back(&f, "a/b/c.txt"), "hello\n");
}

#[test]
fn write_file_refuses_to_overwrite_a_file_that_was_not_read() {
    let f = fixture();
    seed(&f, "notes.txt", "keep me\n");
    let out = call(&f, "write_file", json!({"path": "notes.txt", "content": "replaced\n"}));
    assert!(out.is_error && out.text.contains("was not read in this session"), "{}", out.text);
    assert_eq!(read_back(&f, "notes.txt"), "keep me\n");
}

#[test]
fn write_file_overwrites_after_a_read_and_shows_a_diff() {
    let f = fixture();
    seed(&f, "notes.txt", "one\ntwo\n");
    assert!(!call(&f, "read_file", json!({"path": "notes.txt"})).is_error);
    let out = call(&f, "write_file", json!({"path": "notes.txt", "content": "one\n2\n"}));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("-two") && out.text.contains("+2"), "{}", out.text);
    assert_eq!(read_back(&f, "notes.txt"), "one\n2\n");
}

#[test]
fn write_file_refuses_a_file_changed_since_it_was_read() {
    let f = fixture();
    seed(&f, "notes.txt", "original\n");
    call(&f, "read_file", json!({"path": "notes.txt"}));
    seed(&f, "notes.txt", "changed by someone else\n");
    let out = call(&f, "write_file", json!({"path": "notes.txt", "content": "mine\n"}));
    assert!(out.is_error && out.text.contains("changed since you read it"), "{}", out.text);
    assert_eq!(read_back(&f, "notes.txt"), "changed by someone else\n");
}

#[test]
fn writes_into_git_and_secrets_are_refused() {
    let f = fixture();
    assert!(call(&f, "write_file", json!({"path": ".git/config", "content": "x"})).is_error);
    let out = call(&f, "write_file", json!({"path": ".env", "content": "KEY=1"}));
    assert!(out.is_error && out.text.contains("secret"), "{}", out.text);
    assert!(!f.dir.path().join(".env").exists());
}

#[test]
fn edit_replaces_a_unique_match_and_reports_the_change() {
    let f = fixture();
    seed(&f, "src/lib.rs", "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n");
    call(&f, "read_file", json!({"path": "src/lib.rs"}));
    let out = call(&f, "edit_file", json!({"path": "src/lib.rs", "old_text": "    2\n", "new_text": "    3\n"}));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("1 replacement(s), 2 line(s) changed"), "{}", out.text);
    assert_eq!(read_back(&f, "src/lib.rs"), "fn alpha() {\n    1\n}\n\nfn beta() {\n    3\n}\n");
}

#[test]
fn edit_with_no_match_gives_the_closest_line() {
    let f = fixture();
    seed(&f, "a.rs", "fn compute_total(items: &[u32]) -> u32 {\n    items.iter().sum()\n}\n");
    call(&f, "read_file", json!({"path": "a.rs"}));
    let out = call(&f, "edit_file", json!({"path": "a.rs", "old_text": "fn compute_totals(items: &[u32]) -> u32 {", "new_text": "x"}));
    assert!(out.is_error && out.text.contains("not found"), "{}", out.text);
    assert!(out.text.contains("closest line is 1"), "{}", out.text);
}

#[test]
fn edit_refuses_several_matches_unless_replace_all() {
    let f = fixture();
    seed(&f, "a.txt", "x = 1\ny = 1\n");
    call(&f, "read_file", json!({"path": "a.txt"}));
    let out = call(&f, "edit_file", json!({"path": "a.txt", "old_text": "= 1", "new_text": "= 2"}));
    assert!(out.is_error && out.text.contains("matches 2 places"), "{}", out.text);
    assert_eq!(read_back(&f, "a.txt"), "x = 1\ny = 1\n", "a refused edit must not change the file");

    let all = call(&f, "edit_file", json!({"path": "a.txt", "old_text": "= 1", "new_text": "= 2", "replace_all": true}));
    assert!(!all.is_error, "{}", all.text);
    assert_eq!(read_back(&f, "a.txt"), "x = 2\ny = 2\n");
}

#[test]
fn edit_refuses_a_file_changed_since_it_was_read() {
    let f = fixture();
    seed(&f, "a.txt", "alpha\n");
    call(&f, "read_file", json!({"path": "a.txt"}));
    seed(&f, "a.txt", "beta\n");
    let out = call(&f, "edit_file", json!({"path": "a.txt", "old_text": "beta", "new_text": "gamma"}));
    assert!(out.is_error && out.text.contains("changed since you read it"), "{}", out.text);
    assert_eq!(read_back(&f, "a.txt"), "beta\n");
}

#[test]
fn edit_needs_a_prior_read() {
    let f = fixture();
    seed(&f, "a.txt", "alpha\n");
    let out = call(&f, "edit_file", json!({"path": "a.txt", "old_text": "alpha", "new_text": "beta"}));
    assert!(out.is_error && out.text.contains("was not read in this session"), "{}", out.text);
}

#[test]
fn edit_keeps_crlf_line_endings_when_the_model_writes_lf() {
    let f = fixture();
    seed(&f, "win.txt", "one\r\ntwo\r\nthree\r\n");
    call(&f, "read_file", json!({"path": "win.txt"}));
    let out = call(&f, "edit_file", json!({"path": "win.txt", "old_text": "one\ntwo\n", "new_text": "1\n2\nextra\n"}));
    assert!(!out.is_error, "{}", out.text);
    let after = read_back(&f, "win.txt");
    assert_eq!(after, "1\r\n2\r\nextra\r\nthree\r\n");
    assert_eq!(after.matches('\n').count(), after.matches("\r\n").count(), "no bare LF may appear");
}

#[test]
fn edit_preserves_file_permissions() {
    let f = fixture();
    seed(&f, "run.sh", "echo a\n");
    let path = f.dir.path().join("run.sh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    call(&f, "read_file", json!({"path": "run.sh"}));
    let out = call(&f, "edit_file", json!({"path": "run.sh", "old_text": "echo a", "new_text": "echo b"}));
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o750);
}

#[test]
fn a_failed_write_leaves_the_old_file_whole() {
    let f = fixture();
    seed(&f, "sub/keep.txt", "untouched content\n");
    call(&f, "read_file", json!({"path": "sub/keep.txt"}));
    let dir = f.dir.path().join("sub");
    // Read and traverse still work, but no temporary file can be created.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = call(&f, "write_file", json!({"path": "sub/keep.txt", "content": "new content\n"}));
    let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if out.is_error {
        assert_eq!(read_back(&f, "sub/keep.txt"), "untouched content\n");
        assert_eq!(leftovers.len(), 1, "no temporary file may be left behind: {leftovers:?}");
    } else {
        // Running as root, the directory mode does not stop the write. Then the new content must be whole.
        assert_eq!(read_back(&f, "sub/keep.txt"), "new content\n");
    }
}

#[test]
fn atomic_write_creates_missing_parents() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("x/y/z.txt");
    write::atomic_write(&target, b"ok").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"ok");
    assert!(Path::new(&target).exists());
}
