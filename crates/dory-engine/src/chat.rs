//! Renders chat messages into the prompt format the GGUF ships with.
//!
//! This renders the template directly instead of through minijinja. minijinja 3.0.5 does handle
//! `namespace()` and attribute assignment, but it has no string `.split()` method (the template
//! calls `.split('</think>')`), so it cannot render this template unmodified. The model's template
//! text is checked against a SHA-256 pin first, so a different template fails loudly instead of
//! rendering wrong. Output is validated against `llama-server`'s `/apply-template` (see
//! `scripts/compare_reference.py`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// SHA-256 of `tokenizer.chat_template` in `NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf`.
pub const TEMPLATE_SHA256: &str = "82753bef5cedc4932c1ed509b5c9a12be680fd86d1adb65bc3f7398d11c8eebc";

/// Starts every user turn. The server finds user messages by matching these tokens, so it is shared.
pub const USER_TURN: &str = "<|im_start|>user\n";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Reasoning from a previous assistant turn, rendered inside `<think>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// A tool as the template sees it: `name`, `description` and a JSON-schema `parameters` object.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Value,
}

pub fn check_template(template: &str) -> Result<()> {
    let got = crate::verify::sha256_bytes(template.as_bytes());
    if got != TEMPLATE_SHA256 {
        return Err(Error::Template(format!(
            "expected the Dory template (sha256 {TEMPLATE_SHA256}), got {got}"
        )));
    }
    Ok(())
}

/// Renders the prompt for a conversation that should end with an open assistant turn.
pub fn render(messages: &[Message], tools: &[ToolDef], enable_thinking: bool) -> Result<String> {
    if messages.is_empty() {
        return Err(Error::Template("no messages to render".into()));
    }
    let mut out = String::new();

    // The template takes a leading system message as `system_message`, otherwise "".
    let (system_message, loop_messages) = if messages[0].role == "system" {
        (messages[0].content.as_str(), &messages[1..])
    } else {
        ("", messages)
    };
    // Index of the last user message, relative to loop_messages.
    let last_user_idx: isize = loop_messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i as isize)
        .next_back()
        .unwrap_or(-1);

    out.push_str("<|im_start|>system\n");
    out.push_str(system_message);
    if !tools.is_empty() {
        if !system_message.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(TOOLS_HEADER);
        for tool in tools {
            render_tool(&mut out, tool);
        }
        out.push_str("\n</tools>");
        out.push_str(TOOLS_FOOTER);
    }
    out.push_str("<|im_end|>\n");

    for (i, m) in loop_messages.iter().enumerate() {
        let i = i as isize;
        // `loop.previtem` / `loop.nextitem` are absent at the ends of the list.
        let prev_role = if i > 0 { Some(loop_messages[i as usize - 1].role.as_str()) } else { None };
        let next_role = loop_messages.get(i as usize + 1).map(|n| n.role.as_str());
        let is_last = next_role.is_none();
        match m.role.as_str() {
            "assistant" => render_assistant(&mut out, m, i, last_user_idx),
            "user" | "system" => {
                out.push_str("<|im_start|>");
                out.push_str(&m.role);
                out.push('\n');
                out.push_str(&m.content);
                out.push_str("<|im_end|>\n");
            }
            "tool" => {
                if prev_role.is_some_and(|r| r != "tool") {
                    out.push_str(USER_TURN);
                }
                out.push_str("<tool_response>\n");
                out.push_str(&m.content);
                out.push_str("\n</tool_response>\n");
                if !is_last && next_role != Some("tool") {
                    out.push_str("<|im_end|>\n");
                } else if is_last {
                    out.push_str("<|im_end|>\n");
                }
            }
            other => {
                out.push_str("<|im_start|>");
                out.push_str(other);
                out.push('\n');
                out.push_str(&m.content);
                out.push_str("<|im_end|>\n");
            }
        }
    }

    if enable_thinking {
        out.push_str("<|im_start|>assistant\n<think>\n");
    } else {
        out.push_str("<|im_start|>assistant\n<think></think>");
    }
    Ok(out)
}

const TOOLS_HEADER: &str = "# Tools\n\nYou have access to the following functions:\n\n<tools>";

const TOOLS_FOOTER: &str = "\n\nIf you choose to call a function ONLY reply in the following format with NO suffix:\n\n<tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n<parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\nmultiple lines\n</parameter>\n</function>\n</tool_call>\n\n<IMPORTANT>\nReminder:\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\n- Required parameters MUST be specified\n- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after\n- If there is no function call available, answer the question like normal with your current knowledge and do not tell the user about function calls\n</IMPORTANT>";

fn render_tool(out: &mut String, tool: &ToolDef) {
    out.push_str("\n<function>\n<name>");
    out.push_str(&tool.name);
    out.push_str("</name>");
    if let Some(desc) = &tool.description {
        out.push_str("\n<description>");
        out.push_str(desc.trim());
        out.push_str("</description>");
    }
    out.push_str("\n<parameters>");
    let params = tool.parameters.as_object();
    if let Some(props) = params.and_then(|p| p.get("properties")).and_then(Value::as_object) {
        for (name, fields) in props {
            out.push_str("\n<parameter>\n<name>");
            out.push_str(name);
            out.push_str("</name>");
            if let Some(ty) = fields.get("type") {
                out.push_str("\n<type>");
                out.push_str(&value_to_string(ty));
                out.push_str("</type>");
            }
            if let Some(desc) = fields.get("description") {
                out.push_str("\n<description>");
                out.push_str(value_to_string(desc).trim());
                out.push_str("</description>");
            }
            if let Some(en) = fields.get("enum") {
                out.push_str("\n<enum>");
                out.push_str(&to_json(en));
                out.push_str("</enum>");
            }
            render_extra_keys(out, fields.as_object(), &["name", "type", "description", "enum"]);
            out.push_str("\n</parameter>");
        }
    }
    render_extra_keys(out, params, &["type", "properties", "required"]);
    if let Some(req) = params.and_then(|p| p.get("required")) {
        out.push_str("\n<required>");
        out.push_str(&to_json(req));
        out.push_str("</required>");
    }
    out.push_str("\n</parameters>");
    let tool_obj = serde_json::json!({ "name": tool.name, "description": tool.description, "parameters": tool.parameters });
    render_extra_keys(out, tool_obj.as_object(), &["type", "name", "description", "parameters"]);
    out.push_str("\n</function>");
}

/// Mirrors the template's `render_extra_keys` macro: any key not already handled is emitted.
fn render_extra_keys(out: &mut String, dict: Option<&Map<String, Value>>, handled: &[&str]) {
    let Some(dict) = dict else { return };
    for (key, value) in dict {
        if handled.contains(&key.as_str()) {
            continue;
        }
        let rendered = if value.is_object() || value.is_array() { to_json(value) } else { value_to_string(value) };
        out.push_str("\n<");
        out.push_str(key);
        out.push('>');
        out.push_str(&rendered);
        out.push_str("</");
        out.push_str(key);
        out.push('>');
    }
}

/// The template truncates reasoning in assistant turns before the last user message
/// (`truncate_history_thinking`, default true). `llama-server` does not: its rendering keeps the
/// reasoning in every turn (checked on the reference build, see `scripts/compare_reference.py`),
/// so the harness does the same to produce identical prompts. Set to true for the template's
/// intended behaviour; the comparison script will then report the difference.
const TRUNCATE_HISTORY_THINKING: bool = false;

fn render_assistant(out: &mut String, m: &Message, i: isize, last_user_idx: isize) {
    let old_turn = TRUNCATE_HISTORY_THINKING && i < last_user_idx;

    // Template: reasoning goes in <think>; otherwise an empty <think></think> is prepended.
    let content = match m.reasoning_content.as_deref() {
        Some(r) if !r.trim().is_empty() => format!("<think>\n{r}</think>{}", m.content),
        _ => {
            if m.content.contains("<think>") || m.content.contains("</think>") {
                m.content.clone()
            } else {
                format!("<think></think>{}", m.content)
            }
        }
    };

    if !m.tool_calls.is_empty() {
        out.push_str("<|im_start|>assistant\n");
        if !content.trim().is_empty() {
            if !old_turn {
                out.push_str(content.trim());
                out.push('\n');
            } else {
                let mut c: &str = &content;
                if let Some(pos) = c.rfind("</think>") {
                    c = &c[pos + "</think>".len()..];
                } else if let Some(pos) = c.find("<think>") {
                    c = &c[..pos];
                }
                out.push_str("<think></think>");
                out.push_str(c);
                out.push('\n');
            }
        } else {
            out.push_str("<think></think>");
        }
        for call in &m.tool_calls {
            out.push_str("<tool_call>\n<function=");
            out.push_str(&call.name);
            out.push_str(">\n");
            for (arg, value) in &call.arguments {
                out.push_str("<parameter=");
                out.push_str(arg);
                out.push_str(">\n");
                out.push_str(&value_to_string(value));
                out.push_str("\n</parameter>\n");
            }
            out.push_str("</function>\n</tool_call>\n");
        }
        out.push_str("<|im_end|>\n");
        return;
    }

    if !old_turn {
        out.push_str("<|im_start|>assistant\n");
        out.push_str(content.trim());
        out.push_str("<|im_end|>\n");
    } else {
        let mut c: String = content.clone();
        if c.contains("<think>") && c.contains("</think>") {
            let tail = c.rsplit("</think>").next().unwrap_or("");
            c = format!("<think></think>{tail}");
        }
        let c = c.trim();
        if c.is_empty() {
            out.push_str("<|im_start|>assistant\n<|im_end|>\n");
        } else {
            out.push_str("<|im_start|>assistant\n");
            out.push_str(c);
            out.push_str("<|im_end|>\n");
        }
    }
}

/// Matches `value | string` for scalars and `tojson` for containers.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => n.to_string(),
        other => to_json(other),
    }
}

/// Serializes like the template's `tojson`, which uses Python's separators: `{"a": 1, "b": [2, 3]}`.
fn to_json(v: &Value) -> String {
    let mut out = String::new();
    write_py_json(&mut out, v);
    out
}

fn write_py_json(out: &mut String, v: &Value) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                write_py_json(out, item);
            }
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}
