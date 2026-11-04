//! Keeps a conversation inside the context window.
//!
//! Three stages, applied only as far as needed, in this order:
//! 1. drop the reasoning of old assistant turns (it is large and not needed later);
//! 2. replace the output of old tool calls with a one-line stub that says how to get it back;
//! 3. replace the older turns with a summary the model writes itself.
//!
//! The first user message (the task) and the system message are never changed. Every assistant
//! message that calls tools is still followed by its tool messages.

use dory_engine::chat::{Message, ToolCall};

/// Assistant turns whose reasoning is kept, counted from the end.
pub const KEEP_REASONING_TURNS: usize = 2;
/// Tool results kept whole, counted from the end.
pub const KEEP_RESULTS: usize = 2;

pub const SUMMARY_REQUEST: &str = "Summarize the work so far for yourself: the task, what you have found, what you have changed, and what remains. Under 300 words. Do not call any tool.";

/// Stage 1. Clears the reasoning of every assistant turn except the last `keep` ones.
/// Returns how many turns were changed.
pub fn strip_old_reasoning(messages: &mut [Message], keep: usize) -> usize {
    let assistants: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
        .map(|(i, _)| i)
        .collect();
    let cut = assistants.len().saturating_sub(keep);
    let mut changed = 0;
    for &i in &assistants[..cut] {
        if messages[i].reasoning_content.take().is_some() {
            changed += 1;
        }
    }
    changed
}

/// The tool each `tool` message answers, found by position: a tool message belongs to the most
/// recent assistant message that made calls, in call order.
fn answered_calls(messages: &[Message]) -> Vec<Option<ToolCall>> {
    let mut out = Vec::with_capacity(messages.len());
    let mut pending: Vec<ToolCall> = Vec::new();
    let mut next = 0usize;
    for m in messages {
        match m.role.as_str() {
            "assistant" => {
                pending = m.tool_calls.clone();
                next = 0;
                out.push(None);
            }
            "tool" => {
                out.push(pending.get(next).cloned());
                next += 1;
            }
            _ => {
                pending.clear();
                next = 0;
                out.push(None);
            }
        }
    }
    out
}

/// Characters of a call's arguments kept in a stub.
const STUB_ARG_CHARS: usize = 80;

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

/// Whether a tool result reports a failure. Those are kept even when they are old.
fn is_failure(text: &str) -> bool {
    text.starts_with("error:")
        || text.contains("\n[exit ")
        || (text.starts_with("exit code ") && !text.starts_with("exit code 0"))
}

/// Stage 2. Replaces old tool results with a stub. Keeps the last `keep` results and any failure.
/// Returns how many results were replaced.
pub fn stub_old_results(messages: &mut [Message], keep: usize) -> usize {
    let calls = answered_calls(messages);
    let results: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "tool")
        .map(|(i, _)| i)
        .collect();
    let cut = results.len().saturating_sub(keep);
    let mut stubbed = 0;
    for &i in &results[..cut] {
        if is_failure(&messages[i].content) || messages[i].content.starts_with("[output removed") {
            continue;
        }
        // The stub names the call. Its arguments are cut short: a write_file call carries a whole
        // file, and the stub must be smaller than what it replaces.
        let what = match &calls[i] {
            Some(call) => {
                let args = serde_json::Value::Object(call.arguments.clone()).to_string();
                format!("{} {}", call.name, clip(&args, STUB_ARG_CHARS))
            }
            None => "a tool".to_string(),
        };
        messages[i].content =
            format!("[output removed to save space: {what}; run it again if needed]");
        stubbed += 1;
    }
    stubbed
}

/// Stage 3. Replaces the turns between the task and the last `KEEP_REASONING_TURNS` assistant turns
/// with `summary`. Returns `None` when there is nothing to replace.
pub fn with_summary(messages: &[Message], summary: &str) -> Option<Vec<Message>> {
    let task = messages.iter().position(|m| m.role == "user")?;
    let assistants: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
        .map(|(i, _)| i)
        .collect();
    if assistants.len() <= KEEP_REASONING_TURNS {
        return None;
    }
    let split = assistants[assistants.len() - KEEP_REASONING_TURNS];
    if split <= task + 1 {
        return None;
    }
    let mut out: Vec<Message> = messages[..=task].to_vec();
    out.push(Message::new(
        "user",
        &format!("[Summary of the earlier steps. It is the harness's summary, not a verbatim record.]\n{}", summary.trim()),
    ));
    out.extend_from_slice(&messages[split..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};

    fn call(name: &str, arg: &str) -> ToolCall {
        let mut arguments = Map::new();
        arguments.insert("path".into(), Value::String(arg.into()));
        ToolCall {
            name: name.into(),
            arguments,
        }
    }

    fn assistant(content: &str, reasoning: Option<&str>, calls: Vec<ToolCall>) -> Message {
        Message {
            role: "assistant".into(),
            content: content.into(),
            reasoning_content: reasoning.map(str::to_string),
            tool_calls: calls,
        }
    }

    fn sample() -> Vec<Message> {
        vec![
            Message::new("system", "sys"),
            Message::new("user", "TASK: fix the bug"),
            assistant("", Some("think 1"), vec![call("read_file", "a.rs")]),
            Message::new("tool", "old file body"),
            assistant("", Some("think 2"), vec![call("read_file", "b.rs")]),
            Message::new("tool", "error: not found"),
            assistant("", Some("think 3"), vec![call("search_files", "x")]),
            Message::new("tool", "newest result"),
            assistant("", Some("think 4"), vec![call("run_command", "cargo test")]),
            Message::new("tool", "last result"),
        ]
    }

    #[test]
    fn stage_one_keeps_reasoning_of_the_last_turns_only() {
        let mut m = sample();
        assert_eq!(strip_old_reasoning(&mut m, KEEP_REASONING_TURNS), 2);
        assert!(m[2].reasoning_content.is_none());
        assert!(m[4].reasoning_content.is_none());
        assert_eq!(m[6].reasoning_content.as_deref(), Some("think 3"));
        assert_eq!(m[8].reasoning_content.as_deref(), Some("think 4"));
    }

    #[test]
    fn stage_two_stubs_old_results_and_keeps_recent_and_failures() {
        let mut m = sample();
        let n = stub_old_results(&mut m, KEEP_RESULTS);
        assert_eq!(n, 1, "only the old success is stubbed; the failure is kept");
        assert_eq!(
            m[3].content,
            r#"[output removed to save space: read_file {"path":"a.rs"}; run it again if needed]"#
        );
        assert_eq!(m[5].content, "error: not found");
        assert_eq!(m[7].content, "newest result");
        assert_eq!(m[9].content, "last result");
    }

    #[test]
    fn a_stub_is_shorter_than_a_call_with_a_large_argument() {
        let big = "x".repeat(5000);
        let mut arguments = Map::new();
        arguments.insert("content".into(), Value::String(big.clone()));
        let mut m = vec![
            Message::new("user", "task"),
            assistant(
                "",
                None,
                vec![ToolCall {
                    name: "write_file".into(),
                    arguments,
                }],
            ),
            Message::new("tool", &big),
            assistant("", None, vec![call("read_file", "a")]),
            Message::new("tool", "one"),
            assistant("", None, vec![call("read_file", "b")]),
            Message::new("tool", "two"),
        ];
        let before: usize = m[2].content.len();
        assert_eq!(stub_old_results(&mut m, KEEP_RESULTS), 1);
        assert!(
            m[2].content.len() < before / 10,
            "stub is {} bytes",
            m[2].content.len()
        );
    }

    #[test]
    fn stubbing_keeps_roles_and_call_pairing() {
        let mut m = sample();
        stub_old_results(&mut m, KEEP_RESULTS);
        let roles: Vec<&str> = m.iter().map(|x| x.role.as_str()).collect();
        assert_eq!(
            roles,
            [
                "system",
                "user",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "assistant",
                "tool"
            ]
        );
    }

    #[test]
    fn stage_three_keeps_the_task_and_the_recent_turns() {
        let m = sample();
        let out = with_summary(&m, "Found the bug in a.rs.").unwrap();
        assert_eq!(out[0].content, "sys");
        assert_eq!(out[1].content, "TASK: fix the bug");
        assert!(out[2].content.contains("Found the bug in a.rs."));
        assert!(out[2].content.contains("not a verbatim record"));
        // The last two assistant turns and their results follow, whole.
        assert_eq!(out[3].role, "assistant");
        assert_eq!(out[3].tool_calls[0].name, "search_files");
        assert_eq!(out.last().unwrap().content, "last result");
    }

    #[test]
    fn stage_three_needs_something_to_replace() {
        let m = vec![Message::new("user", "hi"), assistant("hello", None, vec![])];
        assert!(with_summary(&m, "x").is_none());
    }

    #[test]
    fn answered_calls_pairs_results_with_their_calls() {
        let m = sample();
        let pairs = answered_calls(&m);
        assert_eq!(pairs[3].as_ref().unwrap().name, "read_file");
        assert_eq!(pairs[5].as_ref().unwrap().name, "read_file");
        assert!(pairs[0].is_none());
    }
}
