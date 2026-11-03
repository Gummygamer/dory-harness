//! Tool allowlist behaviour. These tests run no model and no GPU work.

use dory_engine::tools::{ToolEntry, ToolSet};
use serde_json::{Map, Value, json};

fn set() -> ToolSet {
    ToolSet {
        entries: vec![
            ToolEntry {
                name: "echo_city".into(),
                description: None,
                parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
                exec: Some(vec!["cat".into()]),
            },
            ToolEntry {
                name: "advertised_only".into(),
                description: None,
                parameters: json!({"type": "object", "properties": {}}),
                exec: None,
            },
        ],
    }
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn unknown_tool_is_refused() {
    let out = set().call("rm_rf", &args(json!({})));
    assert!(out.starts_with("error:") && out.contains("unknown tool"), "{out}");
}

#[test]
fn advertised_tool_without_exec_is_never_run() {
    let out = set().call("advertised_only", &args(json!({})));
    assert!(out.contains("no exec entry"), "{out}");
}

#[test]
fn missing_required_argument_is_reported_and_not_run() {
    let out = set().call("echo_city", &args(json!({"town": "Lisbon"})));
    assert!(out.contains("missing required argument 'city'"), "{out}");
}

#[test]
fn allowed_tool_gets_arguments_as_json_on_stdin() {
    let out = set().call("echo_city", &args(json!({"city": "Lisbon"})));
    assert_eq!(out, r#"{"city":"Lisbon"}"#);
}

#[test]
fn tools_file_loads_and_exposes_definitions_without_exec() {
    let dir = std::env::temp_dir().join(format!("dory-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tools.json");
    std::fs::write(&path, r#"[{"name":"t","parameters":{"type":"object","properties":{}},"exec":["true"]}]"#).unwrap();
    let loaded = ToolSet::load(&path).unwrap();
    let defs = loaded.defs();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "t");
    let _ = std::fs::remove_dir_all(&dir);
}
