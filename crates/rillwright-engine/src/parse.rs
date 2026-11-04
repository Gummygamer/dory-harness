//! Streaming parser for Dory output.
//!
//! The prompt already ends in `<think>\n` when thinking is on, so the stream starts inside
//! reasoning and ends it at `</think>`. Tool calls use the Qwen-style XML format:
//! `<tool_call>\n<function=NAME>\n<parameter=K>\nV\n</parameter>\n</function>\n</tool_call>`.
//!
//! Tags can be split across token pieces, so any tail that could still become a tag is held
//! back until the next piece arrives. Arguments are typed using the tool's JSON schema.

use std::collections::HashMap;

use serde_json::{Map, Value};

const THINK_END: &str = "</think>";
const TOOL_START: &str = "<tool_call>";
const TOOL_END: &str = "</tool_call>";
const PARAM_END: &str = "</parameter>";

#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Reasoning(String),
    Content(String),
    ToolCall {
        name: String,
        arguments: Map<String, Value>,
    },
    /// A tool call that never closed or could not be parsed. Carries the raw text.
    MalformedToolCall(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Reasoning,
    Content,
    ToolCall,
}

pub struct StreamParser {
    state: State,
    buf: String,
    /// Parameter schemas by tool name, used to type argument values.
    schemas: HashMap<String, Value>,
}

impl StreamParser {
    /// `thinking` must match the `enable_thinking` value used to render the prompt.
    pub fn new(thinking: bool, schemas: HashMap<String, Value>) -> Self {
        StreamParser {
            state: if thinking {
                State::Reasoning
            } else {
                State::Content
            },
            buf: String::new(),
            schemas,
        }
    }

    pub fn push(&mut self, text: &str) -> Vec<Piece> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        loop {
            match self.state {
                State::Reasoning => {
                    if let Some(i) = self.buf.find(THINK_END) {
                        let before = self.buf[..i].to_string();
                        push_text(&mut out, Piece::Reasoning, before);
                        self.buf.drain(..i + THINK_END.len());
                        self.state = State::Content;
                        continue;
                    }
                    let keep = partial_suffix(&self.buf, THINK_END);
                    let emit = self.buf.len() - keep;
                    let before = self.buf[..emit].to_string();
                    push_text(&mut out, Piece::Reasoning, before);
                    self.buf.drain(..emit);
                    break;
                }
                State::Content => {
                    if let Some(i) = self.buf.find(TOOL_START) {
                        let before = self.buf[..i].to_string();
                        push_text(&mut out, Piece::Content, before);
                        self.buf.drain(..i + TOOL_START.len());
                        self.state = State::ToolCall;
                        continue;
                    }
                    let keep = partial_suffix(&self.buf, TOOL_START);
                    let emit = self.buf.len() - keep;
                    let before = self.buf[..emit].to_string();
                    push_text(&mut out, Piece::Content, before);
                    self.buf.drain(..emit);
                    break;
                }
                State::ToolCall => {
                    if let Some(i) = find_call_end(&self.buf) {
                        let block = self.buf[..i].to_string();
                        self.buf.drain(..i + TOOL_END.len());
                        out.push(self.parse_call(&block));
                        self.state = State::Content;
                        continue;
                    }
                    // Arguments are buffered until the call closes, so nothing is emitted yet.
                    break;
                }
            }
        }
        out
    }

    /// Flushes whatever is left when generation stops.
    pub fn finish(&mut self) -> Vec<Piece> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.buf);
        match self.state {
            State::Reasoning => push_text(&mut out, Piece::Reasoning, rest),
            State::Content => push_text(&mut out, Piece::Content, rest),
            State::ToolCall => {
                let raw = format!("{TOOL_START}{rest}");
                out.push(Piece::MalformedToolCall(raw));
            }
        }
        self.state = State::Content;
        out
    }

    fn parse_call(&self, block: &str) -> Piece {
        let raw = format!("{TOOL_START}{block}{TOOL_END}");
        match self.parse_block(block) {
            Some((name, arguments)) => Piece::ToolCall { name, arguments },
            None => Piece::MalformedToolCall(raw),
        }
    }

    /// Parses `<function=NAME>` followed by parameters and `</function>`. Anything else is `None`.
    fn parse_block(&self, block: &str) -> Option<(String, Map<String, Value>)> {
        let rest = block.trim_start().strip_prefix("<function=")?;
        let gt = rest.find('>')?;
        let name = rest[..gt].trim().to_string();
        let schema = self.schemas.get(&name);
        let mut arguments = Map::new();
        let mut s = &rest[gt + 1..];
        loop {
            let t = s.trim_start();
            if let Some(after) = t.strip_prefix("</function>") {
                return after.trim().is_empty().then_some((name, arguments));
            }
            let after = t.strip_prefix("<parameter=")?;
            let gt = after.find('>')?;
            let key = after[..gt].trim().to_string();
            let (value, next) = split_value(&after[gt + 1..])?;
            arguments.insert(
                key.clone(),
                typed_value(strip_one_newline(value), param_type(schema, &key)),
            );
            s = next;
        }
    }
}

/// Position of the `</tool_call>` that closes the call in `buf`: the first one directly preceded
/// by `</function>` (plus optional whitespace). A `</tool_call>` anywhere else is part of the text.
fn find_call_end(buf: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = buf[from..].find(TOOL_END) {
        let at = from + i;
        if buf[..at].trim_end().ends_with("</function>") {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Splits a parameter value from the text after it. The value ends at the first `</parameter>`
/// that is followed by optional whitespace and then `<parameter=` or `</function>`. A value that
/// contains the exact closing sequence is ambiguous and is cut at its first occurrence.
fn split_value(s: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(i) = s[from..].find(PARAM_END) {
        let at = from + i;
        let after = &s[at + PARAM_END.len()..];
        let t = after.trim_start();
        if t.starts_with("<parameter=") || t.starts_with("</function>") {
            return Some((&s[..at], after));
        }
        from = at + 1;
    }
    None
}

fn push_text(out: &mut Vec<Piece>, make: fn(String) -> Piece, text: String) {
    if !text.is_empty() {
        out.push(make(text));
    }
}

/// Length of the longest suffix of `buf` that is a proper prefix of `tag`.
fn partial_suffix(buf: &str, tag: &str) -> usize {
    let max = tag.len().min(buf.len());
    (1..=max)
        .rev()
        .find(|&n| buf.is_char_boundary(buf.len() - n) && tag.starts_with(&buf[buf.len() - n..]))
        .unwrap_or(0)
}

fn strip_one_newline(s: &str) -> &str {
    let s = s.strip_prefix('\n').unwrap_or(s);
    s.strip_suffix('\n').unwrap_or(s)
}

fn param_type<'a>(schema: Option<&'a Value>, key: &str) -> Option<&'a str> {
    schema?.get("properties")?.get(key)?.get("type")?.as_str()
}

/// Converts a raw parameter string to the JSON type the schema asks for. Anything that does
/// not parse as that type stays a string, so the model's output is never dropped.
fn typed_value(raw: &str, ty: Option<&str>) -> Value {
    let as_string = || Value::String(raw.to_string());
    match ty {
        Some("integer") => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| as_string()),
        Some("number") => raw
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| as_string()),
        Some("boolean") => match raw.trim() {
            "true" | "True" => Value::Bool(true),
            "false" | "False" => Value::Bool(false),
            _ => as_string(),
        },
        Some("object") | Some("array") => {
            serde_json::from_str(raw.trim()).unwrap_or_else(|_| as_string())
        }
        Some("null") => Value::Null,
        _ => as_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn weather_schemas() -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert(
            "get_weather".to_string(),
            json!({"type":"object","properties":{"city":{"type":"string"},"days":{"type":"integer"},"metric":{"type":"boolean"}},"required":["city"]}),
        );
        m
    }

    fn feed(p: &mut StreamParser, chunks: &[&str]) -> Vec<Piece> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    fn join(pieces: &[Piece], want: fn(&Piece) -> Option<&str>) -> String {
        pieces.iter().filter_map(want).collect()
    }

    fn reasoning(p: &Piece) -> Option<&str> {
        match p {
            Piece::Reasoning(s) => Some(s),
            _ => None,
        }
    }

    fn content(p: &Piece) -> Option<&str> {
        match p {
            Piece::Content(s) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn reasoning_then_content_when_thinking() {
        let mut p = StreamParser::new(true, HashMap::new());
        let out = feed(
            &mut p,
            &["Let me think", "</think>", "\n\nLisbon is sunny."],
        );
        assert_eq!(join(&out, reasoning), "Let me think");
        assert_eq!(join(&out, content), "\n\nLisbon is sunny.");
    }

    #[test]
    fn content_only_when_thinking_off() {
        let mut p = StreamParser::new(false, HashMap::new());
        let out = feed(&mut p, &["Hello"]);
        assert_eq!(out, vec![Piece::Content("Hello".into())]);
    }

    #[test]
    fn split_end_tag_across_pieces_is_not_leaked() {
        let mut p = StreamParser::new(true, HashMap::new());
        let out = feed(&mut p, &["thoughts<", "/th", "ink>answer"]);
        assert_eq!(join(&out, reasoning), "thoughts");
        assert_eq!(join(&out, content), "answer");
    }

    #[test]
    fn tool_call_with_typed_arguments() {
        let mut p = StreamParser::new(false, weather_schemas());
        let text = "Checking.\n<tool_call>\n<function=get_weather>\n<parameter=city>\nLisbon\n</parameter>\n<parameter=days>\n3\n</parameter>\n<parameter=metric>\ntrue\n</parameter>\n</function>\n</tool_call>\n";
        let out = feed(&mut p, &[text]);
        assert_eq!(out[0], Piece::Content("Checking.\n".into()));
        match &out[1] {
            Piece::ToolCall { name, arguments } => {
                assert_eq!(name, "get_weather");
                assert_eq!(arguments["city"], json!("Lisbon"));
                assert_eq!(arguments["days"], json!(3));
                assert_eq!(arguments["metric"], json!(true));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn tool_tag_split_across_tokens() {
        let mut p = StreamParser::new(false, weather_schemas());
        let out = feed(
            &mut p,
            &[
                "<tool",
                "_call>\n<function=get_weather>\n<parameter=city>\nLis",
                "bon\n</parameter>\n</function>",
                "\n</tool_call>",
            ],
        );
        let calls: Vec<_> = out
            .iter()
            .filter(|p| matches!(p, Piece::ToolCall { .. }))
            .collect();
        assert_eq!(calls.len(), 1);
        if let Piece::ToolCall { arguments, .. } = calls[0] {
            assert_eq!(arguments["city"], json!("Lisbon"));
        }
    }

    #[test]
    fn cut_off_tool_call_is_reported_not_dropped() {
        let mut p = StreamParser::new(false, weather_schemas());
        let out = feed(
            &mut p,
            &["<tool_call>\n<function=get_weather>\n<parameter=city>\nLis"],
        );
        assert!(matches!(out.last(), Some(Piece::MalformedToolCall(_))));
    }

    #[test]
    fn unparseable_integer_stays_a_string() {
        assert_eq!(typed_value("three", Some("integer")), json!("three"));
    }

    #[test]
    fn bare_angle_bracket_is_emitted_when_not_a_tag() {
        let mut p = StreamParser::new(false, HashMap::new());
        let out = feed(&mut p, &["a < b and c"]);
        assert_eq!(join(&out, content), "a < b and c");
    }
    /// The exact text the chat renderer emits for one tool call (as the model would produce it).
    fn rendered_call(name: &str, args: &[(&str, &str)]) -> String {
        use crate::chat::{self, Message, ToolCall};
        let arguments = args
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect();
        let call = ToolCall {
            name: name.into(),
            arguments,
        };
        let assistant = Message {
            role: "assistant".into(),
            tool_calls: vec![call],
            ..Default::default()
        };
        let prompt = chat::render(&[Message::new("user", "go"), assistant], &[], false).unwrap();
        let start = prompt.find("<tool_call>").unwrap();
        let end = prompt.find("</tool_call>\n").unwrap() + "</tool_call>\n".len();
        prompt[start..end].to_string()
    }

    fn string_schema(keys: &[&str]) -> HashMap<String, Value> {
        let props: Map<String, Value> = keys
            .iter()
            .map(|k| (k.to_string(), json!({"type": "string"})))
            .collect();
        HashMap::from([(
            "edit".to_string(),
            json!({"type": "object", "properties": props}),
        )])
    }

    /// Parses `text` as the model's output with thinking off; returns the single tool call's arguments.
    fn parse_one(text: &str, keys: &[&str]) -> Option<Map<String, Value>> {
        let mut p = StreamParser::new(false, string_schema(keys));
        let mut pieces = p.push(text);
        pieces.extend(p.finish());
        pieces.iter().find_map(|piece| match piece {
            Piece::ToolCall { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
    }

    #[test]
    fn rendered_values_round_trip_exactly() {
        let values = [
            "plain",
            "",
            "\n\nfn main() {\n    println!(\"hi\");\n}\n\n",
            "a </parameter> in the middle",
            "</function> and </tool_call> alone are fine",
            "<tool_call>\n<function=evil>\n<parameter=x>",
            "<parameter=other>\nvalue",
            "ação — 日本語 🚀 café",
            "trailing spaces   \n\t\n",
        ];
        for v in values {
            let text = rendered_call("edit", &[("content", v)]);
            let args = parse_one(&text, &["content"])
                .unwrap_or_else(|| panic!("no call parsed from {text:?}"));
            assert_eq!(args["content"], json!(v), "round trip failed for {v:?}");
        }
    }

    #[test]
    fn several_parameters_round_trip_in_order() {
        let text = rendered_call(
            "edit",
            &[
                ("path", "src/x.rs"),
                ("content", "a\n</parameter>\nb"),
                ("note", ""),
            ],
        );
        let args = parse_one(&text, &["path", "content", "note"]).unwrap();
        assert_eq!(args["path"], json!("src/x.rs"));
        assert_eq!(args["content"], json!("a\n</parameter>\nb"));
        assert_eq!(args["note"], json!(""));
    }

    #[test]
    fn this_file_round_trips_as_a_parameter() {
        let source = include_str!("parse.rs");
        let text = rendered_call("edit", &[("content", source)]);
        let args = parse_one(&text, &["content"]).expect("call parses");
        assert!(
            args["content"] == json!(source),
            "parse.rs changed in transit"
        );
    }

    #[test]
    fn closing_sequence_inside_a_value_is_a_documented_limit() {
        // `</parameter>` followed by `<parameter=` is indistinguishable from the next parameter.
        // The value is cut there, so the result must differ from what was written.
        let value = "x</parameter>\n<parameter=b>y";
        let text = rendered_call("edit", &[("content", value)]);
        let parsed = parse_one(&text, &["content", "b"]);
        assert_ne!(parsed.map(|a| a["content"].clone()), Some(json!(value)));
    }

    #[test]
    fn call_without_function_close_is_malformed_and_keeps_its_text() {
        let text = "<tool_call>\n<function=get_weather>\n<parameter=city>\nLisbon\n</parameter>\n</tool_call>\nAnd then some text";
        let mut p = StreamParser::new(false, string_schema(&["city"]));
        let mut pieces = p.push(text);
        pieces.extend(p.finish());
        match pieces.last() {
            Some(Piece::MalformedToolCall(raw)) => {
                assert!(raw.contains("And then some text"), "{raw}")
            }
            other => panic!("expected a malformed call, got {other:?}"),
        }
    }
}
