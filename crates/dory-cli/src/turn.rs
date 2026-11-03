//! One user turn: render, generate, parse, and run tools until the model stops calling them.

use std::{
    collections::HashMap,
    io::{Write, stderr, stdout},
};

use dory_engine::{
    Engine, Event, GenParams, GenStats,
    chat::{self, Message, ToolCall},
    parse::{Piece, StreamParser},
    tools::ToolSet,
};
use serde_json::Value;

/// Upper bound on model -> tool -> model rounds for one user message.
pub const MAX_TOOL_ROUNDS: usize = 8;

pub struct TurnOptions<'a> {
    pub engine: &'a Engine,
    pub tools: &'a ToolSet,
    pub thinking: bool,
    pub params: GenParams,
    /// Print reasoning to stderr as it streams.
    pub show_reasoning: bool,
}

#[derive(Debug, Default)]
pub struct TurnReport {
    pub rounds: usize,
    pub tool_calls: usize,
    pub stats: Vec<GenStats>,
}

/// Appends the assistant reply and any tool results to `messages`.
pub fn run_turn(messages: &mut Vec<Message>, opts: &TurnOptions) -> dory_engine::Result<TurnReport> {
    let defs = opts.tools.defs();
    let schemas: HashMap<String, Value> = opts.tools.schemas();
    let mut report = TurnReport::default();

    for _ in 0..MAX_TOOL_ROUNDS {
        report.rounds += 1;
        let prompt = chat::render(messages, &defs, opts.thinking)?;
        let tokens = opts.engine.tokenize(&prompt, false, true)?;

        let mut parser = StreamParser::new(opts.thinking, schemas.clone());
        let mut reasoning = String::new();
        let mut content = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();

        let mut handle = |pieces: Vec<Piece>| {
            for piece in pieces {
                match piece {
                    Piece::Reasoning(t) => {
                        if opts.show_reasoning {
                            let _ = write!(stderr(), "{t}");
                            let _ = stderr().flush();
                        }
                        reasoning.push_str(&t);
                    }
                    Piece::Content(t) => {
                        let _ = write!(stdout(), "{t}");
                        let _ = stdout().flush();
                        content.push_str(&t);
                    }
                    Piece::ToolCall { name, arguments } => {
                        let _ = writeln!(stderr(), "\n[tool call] {name} {}", Value::Object(arguments.clone()));
                        calls.push(ToolCall { name, arguments });
                    }
                    Piece::MalformedToolCall(raw) => {
                        let _ = writeln!(stderr(), "\n[warning] malformed tool call kept as text");
                        content.push_str(&raw);
                    }
                }
            }
        };

        let generation = opts.engine.generate_chat(tokens, opts.params.clone())?;
        let mut done: Option<GenStats> = None;
        for ev in generation {
            match ev? {
                Event::Text(t) => handle(parser.push(&t)),
                Event::Done(s) => done = Some(s),
                Event::Token { .. } => {}
            }
        }
        handle(parser.finish());
        if let Some(s) = done {
            report.stats.push(s);
        }

        let assistant = Message {
            role: "assistant".into(),
            content: content.trim().to_string(),
            reasoning_content: (!reasoning.trim().is_empty()).then(|| reasoning.trim().to_string()),
            tool_calls: calls.clone(),
        };
        messages.push(assistant);

        if calls.is_empty() {
            return Ok(report);
        }
        for call in &calls {
            report.tool_calls += 1;
            let result = opts.tools.call(&call.name, &call.arguments);
            let _ = writeln!(stderr(), "[tool result] {}", truncate(&result, 200));
            messages.push(Message::new("tool", &result));
        }
    }
    let _ = writeln!(stderr(), "\n[stopped after {MAX_TOOL_ROUNDS} tool rounds]");
    Ok(report)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}
