//! The agent loop: render the conversation, generate, parse the reply, run its tool calls, and
//! repeat until the model answers or a limit stops the turn.
//!
//! Every way a round can end is handled here (see the table in `docs/AGENT_TOOLS_PLAN.md`,
//! step 7). The loop never prints: everything a front end may show is sent as an [`AgentEvent`].

use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use dory_engine::{
    Error, Event, Finish, GenParams, GenStats, Result,
    chat::{self, Message, ToolCall, ToolDef},
    parse::{Piece, StreamParser},
};
use serde_json::{Map, Value};

use crate::{
    backend::Backend,
    compact::{KEEP_REASONING_TURNS, KEEP_RESULTS, SUMMARY_REQUEST, strip_old_reasoning, stub_old_results, with_summary},
    policy::{Policy, Verdict},
    tool::Registry,
    tools::sanitize_tool_output,
};

/// Upper bound on model -> tool -> model rounds for one user message, for the plain CLI modes.
pub const DEFAULT_MAX_ROUNDS: usize = 8;
/// Room needed for the summary round, and the most it may write.
const SUMMARY_MIN_TOKENS: usize = 256;
const SUMMARY_MAX_TOKENS: u32 = 512;
/// Malformed calls in a row before the turn gives up.
const MAX_MALFORMED_STREAK: usize = 3;
/// The same call (name and arguments) is refused from this occurrence on.
const REPEAT_REFUSAL_AT: usize = 3;
/// Refusals in one turn before the turn stops.
const MAX_REFUSALS: usize = 2;
/// Length cuts in one turn that discard a partial turn before the turn stops.
const MAX_LENGTH_RETRIES: usize = 1;

const ROUND_LIMIT_NOTE: &str = "The tool round limit has been reached. Answer now with what you have found so far. Do not call any tools.";
const MALFORMED_HELP: &str = "error: this tool call was malformed and was not run. Write each call exactly as:\n<tool_call>\n<function=NAME>\n<parameter=NAME>\nVALUE\n</parameter>\n</function>\n</tool_call>";
const EMPTY_NUDGE: &str = "Your last reply was empty. Answer the question, or call a tool if you need one.";
const REASONING_CALL_NUDGE: &str = "Your tool call was written inside your reasoning, so it was not run. Tool calls must come after your reasoning, in the reply itself.";
const REPEAT_REFUSAL: &str = "error: this exact call has already been made 3 times in this task and was not run. Try a different approach, or answer with what you have.";

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub thinking: bool,
    pub params: GenParams,
    pub max_rounds: usize,
    /// Wall-clock limit for the turn, checked before each round.
    pub max_duration: Option<Duration>,
    /// Tokens kept free after the prompt, so a round never runs into the end of the context.
    pub margin: usize,
    /// Room a round must have left after its prompt. Below this, the conversation is compacted.
    pub min_round_tokens: usize,
    /// The prompt may take this share of the context before it is compacted.
    pub compact_fraction: f64,
}

impl AgentConfig {
    pub fn new(thinking: bool, params: GenParams) -> Self {
        AgentConfig {
            thinking,
            params,
            max_rounds: DEFAULT_MAX_ROUNDS,
            max_duration: None,
            margin: 256,
            min_round_tokens: 2048,
            compact_fraction: 0.7,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    Reasoning(String),
    Content(String),
    ToolCall { name: String, arguments: Map<String, Value> },
    /// A tool call that could not be parsed. Its raw text stays in the assistant message.
    MalformedCall(String),
    /// A message was appended to the conversation. A front end can persist it here.
    MessageAdded(Message),
    /// The output the model will see, after sanitizing.
    ToolResult { name: String, text: String },
    RoundStats(GenStats),
    /// The conversation was shortened. `stage` is 1 (reasoning), 2 (old results) or 3 (summary).
    Compacted { stage: u8, before: usize, after: usize },
    Finished(Reason),
}

/// Why a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The model answered without calling a tool.
    Answered,
    /// `max_rounds` was reached. The model then gets one final round to answer, with calls ignored.
    RoundLimit,
    /// `max_duration` passed before a round started.
    TimeLimit,
    /// Malformed calls three rounds in a row.
    MalformedCalls,
    /// The model gave no answer and no call, even after a nudge.
    EmptyAnswer,
    /// Generation hit `max_tokens` in a way that could not be kept.
    Length,
    Cancelled,
    /// The context cannot hold the conversation and the turn cannot continue.
    ContextFull,
    /// The model kept repeating a call the harness had already refused.
    RepeatedCalls,
}

impl Reason {
    /// The label printed to stderr and written to JSONL.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Answered => "answered",
            Reason::RoundLimit => "round_limit",
            Reason::TimeLimit => "time_limit",
            Reason::MalformedCalls => "malformed_calls",
            Reason::EmptyAnswer => "empty_answer",
            Reason::Length => "length",
            Reason::Cancelled => "cancelled",
            Reason::ContextFull => "context_full",
            Reason::RepeatedCalls => "repeated_calls",
        }
    }
}

#[derive(Debug, Default)]
pub struct TurnReport {
    pub rounds: usize,
    pub tool_calls: usize,
    pub stats: Vec<GenStats>,
}

pub struct Agent<'a> {
    pub backend: &'a dyn Backend,
    pub tools: &'a Registry,
    pub policy: &'a Policy,
    pub config: AgentConfig,
    /// Set from outside (for example by Ctrl-C) to stop the current generation.
    pub cancel: Arc<AtomicBool>,
}

/// What one generation produced, after parsing.
struct Round {
    reasoning: String,
    content: String,
    calls: Vec<ToolCall>,
    malformed: Vec<String>,
    stats: GenStats,
}

/// Counters that last for one user message.
#[derive(Default)]
struct TurnState {
    malformed_streak: usize,
    nudged_empty: bool,
    nudged_reasoning: bool,
    length_retries: usize,
    refusals: usize,
    call_counts: HashMap<String, usize>,
}

impl Agent<'_> {
    /// Appends the assistant replies and tool results to `messages`, until the turn ends.
    pub fn run_turn(&self, messages: &mut Vec<Message>, on_event: &mut dyn FnMut(AgentEvent)) -> Result<TurnReport> {
        let started = Instant::now();
        let defs = self.tools.defs();
        let schemas = self.tools.schemas();
        let mut report = TurnReport::default();
        let mut st = TurnState::default();
        let mut thinking_off_next = false;
        let mut final_round = false;

        loop {
            if self.config.max_duration.is_some_and(|max| started.elapsed() >= max) {
                return finish(on_event, report, Reason::TimeLimit);
            }
            if !final_round && report.rounds >= self.config.max_rounds {
                final_round = true;
                push_message(messages, on_event, Message::new("user", ROUND_LIMIT_NOTE));
            }
            let thinking = self.config.thinking && !std::mem::take(&mut thinking_off_next);
            let seed = self.config.params.seed.wrapping_add(report.rounds as u32);
            report.rounds += 1;

            let mut tokens = self.prompt_tokens(messages, &defs, thinking)?;
            if !self.fits(tokens) && !self.compact(messages, &defs, thinking, false, &mut tokens, on_event)?.0 {
                return finish(on_event, report, Reason::ContextFull);
            }
            let round = match self.generate_round(messages, &defs, &schemas, thinking, seed, self.round_max_tokens(tokens), on_event) {
                Ok(r) => r,
                Err(e) if is_context_error(&e) => {
                    // The engine disagrees with the count. Retry only if compaction changed something.
                    let (fits, changed) = self.compact(messages, &defs, thinking, true, &mut tokens, on_event)?;
                    if !fits || !changed {
                        return finish(on_event, report, Reason::ContextFull);
                    }
                    let retry = self.generate_round(messages, &defs, &schemas, thinking, seed, self.round_max_tokens(tokens), on_event);
                    match retry {
                        Ok(r) => r,
                        Err(e) if is_context_error(&e) => return finish(on_event, report, Reason::ContextFull),
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            };
            on_event(AgentEvent::RoundStats(round.stats.clone()));
            report.stats.push(round.stats.clone());

            // Cancelled: keep what was said, run nothing.
            if round.stats.finish == Finish::Cancelled {
                push_message(messages, on_event, assistant(&round, Vec::new()));
                return finish(on_event, report, Reason::Cancelled);
            }
            // The final round's calls are ignored: the model must answer.
            if final_round {
                if !round.content.trim().is_empty() {
                    push_message(messages, on_event, assistant(&round, Vec::new()));
                }
                return finish(on_event, report, Reason::RoundLimit);
            }
            // Length during reasoning or inside a call: the partial turn is discarded and the round
            // is retried with thinking off. Length inside the content is kept as the answer.
            let cut_in_call = !round.malformed.is_empty();
            let cut_in_reasoning = round.content.trim().is_empty() && round.calls.is_empty();
            if round.stats.finish == Finish::Length && (cut_in_call || cut_in_reasoning) {
                st.length_retries += 1;
                if st.length_retries > MAX_LENGTH_RETRIES {
                    return finish(on_event, report, Reason::Length);
                }
                thinking_off_next = true;
                continue;
            }

            if !round.calls.is_empty() || !round.malformed.is_empty() {
                push_message(messages, on_event, assistant(&round, round.calls.clone()));
                for call in &round.calls {
                    report.tool_calls += 1;
                    let text = self.run_call(call, &mut st, on_event);
                    push_message(messages, on_event, Message::new("tool", &text));
                }
                for _ in &round.malformed {
                    push_message(messages, on_event, Message::new("tool", MALFORMED_HELP));
                }
                if round.malformed.is_empty() {
                    st.malformed_streak = 0;
                } else {
                    st.malformed_streak += 1;
                    if st.malformed_streak >= MAX_MALFORMED_STREAK {
                        return finish(on_event, report, Reason::MalformedCalls);
                    }
                }
                if st.refusals >= MAX_REFUSALS {
                    return finish(on_event, report, Reason::RepeatedCalls);
                }
                continue;
            }

            st.malformed_streak = 0;
            if !round.content.trim().is_empty() {
                push_message(messages, on_event, assistant(&round, Vec::new()));
                // A length cut in the content is kept as the answer.
                let reason = if round.stats.finish == Finish::Length { Reason::Length } else { Reason::Answered };
                return finish(on_event, report, reason);
            }
            // No content and no call. A call written inside reasoning gets one nudge, as does an empty reply.
            let call_in_reasoning = round.reasoning.contains("<tool_call>");
            if call_in_reasoning && !st.nudged_reasoning {
                st.nudged_reasoning = true;
                push_message(messages, on_event, assistant(&round, Vec::new()));
                push_message(messages, on_event, Message::new("user", REASONING_CALL_NUDGE));
                continue;
            }
            if !call_in_reasoning && !st.nudged_empty {
                st.nudged_empty = true;
                push_message(messages, on_event, assistant(&round, Vec::new()));
                push_message(messages, on_event, Message::new("user", EMPTY_NUDGE));
                continue;
            }
            push_message(messages, on_event, assistant(&round, Vec::new()));
            return finish(on_event, report, Reason::EmptyAnswer);
        }
    }

    /// Compacts the conversation now, whether or not it is over the limit (the REPL's `/compact`).
    /// Returns whether any stage changed it.
    pub fn compact_now(&self, messages: &mut Vec<Message>, on_event: &mut dyn FnMut(AgentEvent)) -> Result<bool> {
        let defs = self.tools.defs();
        let mut tokens = self.prompt_tokens(messages, &defs, self.config.thinking)?;
        let (_, changed) = self.compact(messages, &defs, self.config.thinking, true, &mut tokens, on_event)?;
        Ok(changed)
    }

    fn prompt_tokens(&self, messages: &[Message], defs: &[ToolDef], thinking: bool) -> Result<usize> {
        let prompt = chat::render(messages, defs, thinking)?;
        self.backend.count_tokens(&prompt)
    }

    /// Whether a prompt of `tokens` leaves a round its minimum room, and is not too large a share.
    fn fits(&self, tokens: usize) -> bool {
        let n_ctx = self.backend.n_ctx();
        let room = n_ctx.saturating_sub(tokens + self.config.margin);
        (tokens as f64) <= n_ctx as f64 * self.config.compact_fraction && room >= self.config.min_round_tokens
    }

    /// The most a round may generate after a prompt of `tokens`.
    fn round_max_tokens(&self, tokens: usize) -> u32 {
        let room = self.backend.n_ctx().saturating_sub(tokens + self.config.margin);
        (self.config.params.max_tokens as usize).min(room) as u32
    }

    /// Applies the compaction stages in order, each only while the prompt does not fit (or all of
    /// them when `force` is set, after the engine reported a full context). Returns whether the
    /// prompt fits now, and whether any stage changed the conversation.
    fn compact(
        &self,
        messages: &mut Vec<Message>,
        defs: &[ToolDef],
        thinking: bool,
        force: bool,
        tokens: &mut usize,
        on_event: &mut dyn FnMut(AgentEvent),
    ) -> Result<(bool, bool)> {
        let mut changed = false;
        if force || !self.fits(*tokens) {
            let before = *tokens;
            if strip_old_reasoning(messages, KEEP_REASONING_TURNS) > 0 {
                *tokens = self.prompt_tokens(messages, defs, thinking)?;
                changed = true;
                on_event(AgentEvent::Compacted { stage: 1, before, after: *tokens });
            }
        }
        if force || !self.fits(*tokens) {
            let before = *tokens;
            if stub_old_results(messages, KEEP_RESULTS) > 0 {
                *tokens = self.prompt_tokens(messages, defs, thinking)?;
                changed = true;
                on_event(AgentEvent::Compacted { stage: 2, before, after: *tokens });
            }
        }
        if (force || !self.fits(*tokens)) && with_summary(messages, "").is_some() {
            let before = *tokens;
            if let Some(summary) = self.summarize(messages)? {
                if let Some(shorter) = with_summary(messages, &summary) {
                    *messages = shorter;
                    *tokens = self.prompt_tokens(messages, defs, thinking)?;
                    changed = true;
                    on_event(AgentEvent::Compacted { stage: 3, before, after: *tokens });
                }
            }
        }
        Ok((self.fits(*tokens), changed))
    }

    /// One model round, with no tools, that writes a summary of the work so far. `None` if even
    /// that request would not fit.
    fn summarize(&self, messages: &[Message]) -> Result<Option<String>> {
        let mut request = messages.to_vec();
        request.push(Message::new("user", SUMMARY_REQUEST));
        let prompt = chat::render(&request, &[], false)?;
        let n_ctx = self.backend.n_ctx();
        let used = self.backend.count_tokens(&prompt)?;
        let room = n_ctx.saturating_sub(used + self.config.margin);
        if room < SUMMARY_MIN_TOKENS {
            return Ok(None);
        }
        let params = GenParams { max_tokens: (SUMMARY_MAX_TOKENS as usize).min(room) as u32, ..self.config.params.clone() };
        let mut text = String::new();
        self.backend.generate(&prompt, &params, &self.cancel, &mut |ev| {
            if let Event::Text(t) = ev {
                text.push_str(&t);
            }
        })?;
        Ok(Some(text))
    }

    /// Renders, generates and parses one round. Parsed pieces are sent as events as they arrive.
    #[allow(clippy::too_many_arguments)]
    fn generate_round(
        &self,
        messages: &[Message],
        defs: &[ToolDef],
        schemas: &HashMap<String, Value>,
        thinking: bool,
        seed: u32,
        max_tokens: u32,
        on_event: &mut dyn FnMut(AgentEvent),
    ) -> Result<Round> {
        let prompt = chat::render(messages, defs, thinking)?;
        let params = GenParams { seed, max_tokens, ..self.config.params.clone() };
        let mut parser = StreamParser::new(thinking, schemas.clone());
        let mut reasoning = String::new();
        let mut content = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut malformed: Vec<String> = Vec::new();
        let mut handle = |pieces: Vec<Piece>| {
            for piece in pieces {
                match piece {
                    Piece::Reasoning(t) => {
                        reasoning.push_str(&t);
                        on_event(AgentEvent::Reasoning(t));
                    }
                    Piece::Content(t) => {
                        content.push_str(&t);
                        on_event(AgentEvent::Content(t));
                    }
                    Piece::ToolCall { name, arguments } => {
                        on_event(AgentEvent::ToolCall { name: name.clone(), arguments: arguments.clone() });
                        calls.push(ToolCall { name, arguments });
                    }
                    Piece::MalformedToolCall(raw) => {
                        on_event(AgentEvent::MalformedCall(raw.clone()));
                        // Kept as text in the assistant message, as the model wrote it.
                        content.push_str(&raw);
                        malformed.push(raw);
                    }
                }
            }
        };
        let stats = self.backend.generate(&prompt, &params, &self.cancel, &mut |ev| {
            if let Event::Text(t) = ev {
                handle(parser.push(&t));
            }
        })?;
        handle(parser.finish());
        Ok(Round { reasoning, content, calls, malformed, stats })
    }

    /// Checks the call against the policy, then runs it. Returns the text the model will see.
    fn execute(&self, call: &ToolCall) -> String {
        match self.tools.prepare(&call.name, &call.arguments) {
            Err(outcome) => outcome.text,
            Ok(tool) => match self.policy.check(tool, &call.arguments, &self.tools.ctx) {
                Verdict::Refuse(text) => text,
                Verdict::Run => tool.run(&call.arguments, &self.tools.ctx).text,
            },
        }
    }

    /// Runs one call and returns the text the model will see. A call that is repeated too often is
    /// refused, and refusing it again ends the turn.
    fn run_call(&self, call: &ToolCall, st: &mut TurnState, on_event: &mut dyn FnMut(AgentEvent)) -> String {
        let key = format!("{}\u{1}{}", call.name, Value::Object(call.arguments.clone()));
        let seen = st.call_counts.entry(key).or_insert(0);
        *seen += 1;
        let text = if *seen >= REPEAT_REFUSAL_AT {
            st.refusals += 1;
            REPEAT_REFUSAL.to_string()
        } else {
            sanitize_tool_output(&self.execute(call))
        };
        on_event(AgentEvent::ToolResult { name: call.name.clone(), text: text.clone() });
        text
    }
}

/// Appends `m` to the conversation and tells the front end, so it can be persisted before the next
/// step runs.
fn push_message(messages: &mut Vec<Message>, on_event: &mut dyn FnMut(AgentEvent), m: Message) {
    on_event(AgentEvent::MessageAdded(m.clone()));
    messages.push(m);
}

fn assistant(round: &Round, tool_calls: Vec<ToolCall>) -> Message {
    let reasoning = round.reasoning.trim();
    Message {
        role: "assistant".into(),
        content: round.content.trim().to_string(),
        reasoning_content: (!reasoning.is_empty()).then(|| reasoning.to_string()),
        tool_calls,
    }
}

fn finish(on_event: &mut dyn FnMut(AgentEvent), report: TurnReport, reason: Reason) -> Result<TurnReport> {
    on_event(AgentEvent::Finished(reason));
    Ok(report)
}

fn is_context_error(e: &Error) -> bool {
    matches!(e, Error::PromptTooLong { .. } | Error::ContextExhausted { .. })
}
