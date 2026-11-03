//! Read-only tools on a fixture tree: ignored files, binary files, invalid UTF-8, long lines,
//! empty files, and the output limits.

use std::fs;

use dory_agent::{
    Registry, ToolCtx,
    tool::ToolOutcome,
    workspace::Workspace,
};
use dory_agent::builtin::read;
use serde_json::{Map, Value, json};

struct Fixture {
    _dir: tempfile::TempDir,
    reg: Registry,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src/deep")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
    fs::write(root.join("src/lib.rs"), "fn alpha() {}\nfn beta() {}\nlet needle = 1;\n").unwrap();
    fs::write(root.join("src/deep/mod.rs"), "pub fn gamma() { /* needle */ }\n").unwrap();
    fs::write(root.join("target/build.rs"), "needle in ignored output\n").unwrap();
    fs::write(root.join("debug.log"), "needle in an ignored log\n").unwrap();
    fs::write(root.join("empty.txt"), "").unwrap();
    fs::write(root.join("image.bin"), b"\x89PNG\r\n\x1a\n\x00\x00binary").unwrap();
    fs::write(root.join("latin.txt"), b"caf\xe9 needle\n").unwrap();
    fs::write(root.join("long.txt"), format!("{}\n", "x".repeat(5000))).unwrap();
    let many: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
    fs::write(root.join("many.txt"), many).unwrap();
    let mut reg = Registry::new(ToolCtx::new(Workspace::new(root, &[]).unwrap()));
    for t in read::tools() {
        reg.add(t).unwrap();
    }
    Fixture { _dir: dir, reg }
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn call(f: &Fixture, name: &str, v: Value) -> ToolOutcome {
    f.reg.call(name, &args(v))
}

#[test]
fn list_dir_respects_gitignore_and_marks_folders() {
    let f = fixture();
    let out = call(&f, "list_dir", json!({"path": ""}));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("src/"), "{}", out.text);
    assert!(!out.text.contains("target"), "ignored folder listed: {}", out.text);
    assert!(!out.text.contains("debug.log"), "ignored file listed: {}", out.text);
}

#[test]
fn list_dir_depth_goes_deeper() {
    let f = fixture();
    let shallow = call(&f, "list_dir", json!({"path": "src"})).text;
    assert!(!shallow.contains("mod.rs"));
    let deep = call(&f, "list_dir", json!({"path": "src", "depth": 2})).text;
    assert!(deep.contains("deep/mod.rs"), "{deep}");
}

#[test]
fn list_dir_refuses_a_file_and_escapes() {
    let f = fixture();
    assert!(call(&f, "list_dir", json!({"path": "src/lib.rs"})).is_error);
    let out = call(&f, "list_dir", json!({"path": "../"}));
    assert!(out.is_error && out.text.contains("outside"), "{}", out.text);
}

#[test]
fn read_file_numbers_lines_and_reports_the_total() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "src/lib.rs"}));
    assert!(out.text.contains("     1\tfn alpha() {}"), "{}", out.text);
    assert!(out.text.contains("[3 lines in total]"), "{}", out.text);
}

#[test]
fn read_file_pages_with_start_line_and_says_where_to_go_next() {
    let f = fixture();
    let first = call(&f, "read_file", json!({"path": "many.txt", "max_lines": 400})).text;
    assert!(first.contains("[showing lines 1-400 of 1000. Read the rest with start_line=401.]"), "{first}");
    let second = call(&f, "read_file", json!({"path": "many.txt", "start_line": 401, "max_lines": 400})).text;
    assert!(second.contains("   401\tline 401"), "{second}");
}

#[test]
fn read_file_default_is_400_lines() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "many.txt"})).text;
    assert!(out.contains("   400\tline 400"), "{out}");
    assert!(!out.contains("   401\t"), "default must stop at 400 lines");
}

#[test]
fn read_file_refuses_binary_with_its_type_and_size() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "image.bin"}));
    assert!(out.is_error);
    assert!(out.text.contains("PNG image") && out.text.contains("binary"), "{}", out.text);
}

#[test]
fn read_file_marks_invalid_utf8_instead_of_failing() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "latin.txt"}));
    assert!(!out.is_error);
    assert!(out.text.contains("not valid UTF-8"), "{}", out.text);
    assert!(out.text.contains("caf\u{fffd} needle"), "{}", out.text);
}

#[test]
fn read_file_clips_very_long_lines() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "long.txt"})).text;
    assert!(out.contains("[line clipped]"));
    assert!(out.len() < 3000, "a 5000-char line must not be sent whole");
}

#[test]
fn read_file_shows_an_empty_file() {
    let f = fixture();
    assert_eq!(call(&f, "read_file", json!({"path": "empty.txt"})).text, "[empty file]");
}

#[test]
fn read_file_reports_a_start_past_the_end() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "src/lib.rs", "start_line": 99}));
    assert!(out.is_error && out.text.contains("past the end"), "{}", out.text);
}

#[test]
fn unknown_arguments_are_named_in_the_error() {
    let f = fixture();
    let out = call(&f, "read_file", json!({"path": "src/lib.rs", "line": 3}));
    assert!(out.is_error && out.text.contains("has no argument 'line'"), "{}", out.text);
}

#[test]
fn a_missing_required_argument_is_named() {
    let f = fixture();
    let out = call(&f, "read_file", json!({}));
    assert!(out.is_error && out.text.contains("missing required argument 'path'"), "{}", out.text);
}

#[test]
fn search_finds_matches_and_skips_ignored_and_binary_files() {
    let f = fixture();
    let out = call(&f, "search_files", json!({"pattern": "needle"}));
    assert!(out.text.contains("src/lib.rs:3:let needle = 1;"), "{}", out.text);
    assert!(out.text.contains("src/deep/mod.rs:1:"), "{}", out.text);
    assert!(!out.text.contains("target"), "{}", out.text);
    assert!(!out.text.contains("debug.log"), "{}", out.text);
}

#[test]
fn search_never_shows_lines_from_secret_files() {
    let f = fixture();
    fs::write(f._dir.path().join(".env"), "SECRET=sk-FAKE-search\n").unwrap();
    let out = call(&f, "search_files", json!({"pattern": "SECRET"}));
    assert!(!out.text.contains("sk-FAKE-search"), "{}", out.text);
    assert_eq!(out.text, "no matches");
}

#[test]
fn search_glob_and_no_match() {
    let f = fixture();
    let out = call(&f, "search_files", json!({"pattern": "fn ", "glob": "*.rs"})).text;
    assert!(out.contains("src/lib.rs:1:fn alpha"), "{out}");
    assert_eq!(call(&f, "search_files", json!({"pattern": "zzz_absent"})).text, "no matches");
}

#[test]
fn search_caps_the_number_of_results() {
    let f = fixture();
    let out = call(&f, "search_files", json!({"pattern": "line", "max_results": 5})).text;
    assert_eq!(out.lines().filter(|l| l.contains(":")).count(), 5, "{out}");
    assert!(out.contains("more matches not shown"), "{out}");
}

#[test]
fn search_rejects_a_bad_regex() {
    let f = fixture();
    let out = call(&f, "search_files", json!({"pattern": "("}));
    assert!(out.is_error && out.text.contains("invalid regular expression"), "{}", out.text);
}
