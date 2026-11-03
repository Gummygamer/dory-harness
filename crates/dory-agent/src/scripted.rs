//! A backend that replays canned replies. It needs no model and no GPU, so the agent loop can be
//! tested end to end.

use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
};

use dory_engine::{Error, Event, Finish, GenParams, GenStats, Result};

use crate::{
    backend::Backend,
    policy::{Answer, Approver, Request},
};

/// Answers approval requests from a script, and records what it was asked.
pub struct ScriptedApprover {
    answers: RefCell<VecDeque<Answer>>,
    pub asked: RefCell<Vec<Request>>,
}

impl ScriptedApprover {
    pub fn new(answers: Vec<Answer>) -> Self {
        ScriptedApprover { answers: RefCell::new(answers.into()), asked: RefCell::new(Vec::new()) }
    }
}

impl Approver for ScriptedApprover {
    fn ask(&self, request: &Request) -> Answer {
        self.asked.borrow_mut().push(Request { tool: request.tool.clone(), summary: request.summary.clone() });
        self.answers.borrow_mut().pop_front().unwrap_or(Answer::Unavailable)
    }
}

/// Pieces are emitted this many characters at a time, so the parser sees split tags. Each piece
/// counts as one generated token.
const PIECE_CHARS: usize = 7;

/// One scripted generation.
pub enum Reply {
    /// Ends normally, with the EOG token.
    Stop(String),
    /// Hits `max_tokens` after this text.
    Length(String),
    /// Stops early because the caller cancelled.
    Cancelled(String),
    /// Fails before producing anything, as the engine does for a full context.
    Fail(Error),
}

impl From<&str> for Reply {
    fn from(s: &str) -> Self {
        Reply::Stop(s.to_string())
    }
}

pub struct ScriptedBackend {
    replies: RefCell<VecDeque<Reply>>,
    n_ctx: usize,
    /// Every prompt the loop sent, in order.
    pub prompts: RefCell<Vec<String>>,
    /// The seed of every generation, in order.
    pub seeds: RefCell<Vec<u32>>,
    /// The `max_tokens` of every generation, in order.
    pub max_tokens: RefCell<Vec<u32>>,
}

impl ScriptedBackend {
    /// `replies` are the model's raw outputs, one per round, including any `<think>` and
    /// `<tool_call>` markup.
    pub fn new(replies: Vec<&str>) -> Self {
        Self::with_replies(replies.into_iter().map(Reply::from).collect())
    }

    pub fn with_replies(replies: Vec<Reply>) -> Self {
        ScriptedBackend {
            replies: RefCell::new(replies.into()),
            n_ctx: 32768,
            prompts: RefCell::new(Vec::new()),
            seeds: RefCell::new(Vec::new()),
            max_tokens: RefCell::new(Vec::new()),
        }
    }

    /// Sets the context size. Tokens are counted as a quarter of the characters, roughly.
    pub fn with_n_ctx(mut self, n_ctx: usize) -> Self {
        self.n_ctx = n_ctx;
        self
    }

    pub fn remaining(&self) -> usize {
        self.replies.borrow().len()
    }
}

impl Backend for ScriptedBackend {
    fn n_ctx(&self) -> usize {
        self.n_ctx
    }

    fn count_tokens(&self, prompt: &str) -> Result<usize> {
        Ok(prompt.chars().count() / 4 + 1)
    }

    fn generate(
        &self,
        prompt: &str,
        params: &GenParams,
        cancel: &Arc<AtomicBool>,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<GenStats> {
        self.prompts.borrow_mut().push(prompt.to_string());
        self.seeds.borrow_mut().push(params.seed);
        self.max_tokens.borrow_mut().push(params.max_tokens);
        let reply = self
            .replies
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| Error::Tool("scripted backend has no reply left".into()))?;
        let (text, scripted_finish) = match reply {
            Reply::Stop(t) => (t, Finish::Stop),
            Reply::Length(t) => (t, Finish::Length),
            Reply::Cancelled(t) => (t, Finish::Cancelled),
            Reply::Fail(e) => return Err(e),
        };

        let chars: Vec<char> = text.chars().collect();
        let mut generated = 0usize;
        let mut finish = scripted_finish;
        for piece in chars.chunks(PIECE_CHARS) {
            if cancel.load(Ordering::Relaxed) {
                finish = Finish::Cancelled;
                break;
            }
            if generated >= params.max_tokens as usize {
                finish = Finish::Length;
                break;
            }
            on_event(Event::Text(piece.iter().collect()));
            generated += 1;
        }
        Ok(GenStats {
            finish,
            prompt_tokens: prompt.len(),
            reused_tokens: 0,
            chunks: Vec::new(),
            generated,
            prompt_ms: 0.0,
            gen_ms: 0.0,
        })
    }
}
