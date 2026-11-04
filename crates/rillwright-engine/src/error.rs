use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model checksum mismatch for {path}: expected sha256 {expected}, got {actual}")]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error(
        "llama.cpp build mismatch: linked against fork commit {built}, harness is pinned to {pinned}"
    )]
    ForkCommit { built: String, pinned: String },
    #[error("loop count {0} is invalid: Dory supports 1 to 4 loops")]
    InvalidLoops(u32),
    #[error("failed to load model {0}")]
    ModelLoad(PathBuf),
    #[error("failed to create a context with n_ctx={n_ctx} (out of memory?)")]
    ContextInit { n_ctx: u32 },
    #[error("the prompt is empty")]
    EmptyPrompt,
    #[error("prompt is {prompt} tokens but the context holds {n_ctx}; leave room to generate")]
    PromptTooLong { prompt: usize, n_ctx: u32 },
    #[error(
        "context exhausted at {pos}/{n_ctx} tokens: the Dory PR has no context shifting, so the request cannot continue"
    )]
    ContextExhausted { pos: usize, n_ctx: u32 },
    #[error("llama_decode failed with code {0}")]
    Decode(i32),
    #[error("tokenization failed for a {0}-byte input")]
    Tokenize(usize),
    #[error("unsupported chat template: {0}")]
    Template(String),
    #[error("engine thread has stopped")]
    EngineStopped,
    #[error("tool error: {0}")]
    Tool(String),
    #[error("session file error: {0}")]
    Session(String),
}

pub type Result<T> = std::result::Result<T, Error>;
