//! `dory-serve`: an OpenAI-style `/v1/chat/completions` endpoint on top of the Dory engine.
//!
//! Local use only: it binds to a loopback address unless `--allow-remote` is given. The engine
//! handles one generation at a time (a mutex serializes requests), and each request is
//! stateless: the client sends the full message history, as with the OpenAI API. Tool calls are
//! returned to the client, which runs them and sends the results back as `tool` messages.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use clap::Parser;
use dory_agent::tools::sanitize_tool_output;
use dory_engine::{
    CacheMode, Engine, EngineOptions, Error as EngineError, Event, Finish, GenParams, GenStats, LoadOptions,
    PrefillSchedule,
    chat::{self, Message, ToolCall, ToolDef},
    parse::{Piece, StreamParser},
    verify,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Request fields this server understands. Any other field is logged, not silently dropped.
const KNOWN_FIELDS: &[&str] = &[
    "model", "messages", "tools", "tool_choice", "stream", "stream_options", "max_tokens", "temperature", "top_p",
    "seed", "chat_template_kwargs", "n", "user",
];

const MODEL_ID: &str = "dory";

#[derive(Parser)]
#[command(name = "dory-serve", version = env!("DORY_VERSION"), about = "OpenAI-style chat endpoint for Nemotron Dory (local)")]
struct Args {
    /// Path to the GGUF file (its SHA-256 must match the pin). Defaults to $DORY_MODEL.
    #[arg(long, env = "DORY_MODEL")]
    model: PathBuf,
    #[arg(long, default_value_t = 2)]
    loops: u32,
    #[arg(long, value_parser = ["last_loop", "per_loop"], default_value = "last_loop")]
    cache_mode: String,
    #[arg(long, default_value_t = 32768)]
    ctx: u32,
    #[arg(long, default_value_t = 2048)]
    batch: u32,
    #[arg(long, default_value_t = 512)]
    ubatch: u32,
    #[arg(long, default_value_t = 8)]
    threads: i32,
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    /// Required to bind a non-loopback address. The endpoint has no authentication.
    #[arg(long)]
    allow_remote: bool,
    /// Prompt split for prefill: `server` matches llama-server (default), `single` uses full batches.
    #[arg(long, default_value = "server", value_parser = parse_prefill)]
    prefill: PrefillSchedule,
}

fn parse_prefill(s: &str) -> Result<PrefillSchedule, String> {
    s.parse()
}

struct AppState {
    engine: Mutex<Engine>,
    prefill: PrefillSchedule,
}

// ---- request types (OpenAI shapes) ----

#[derive(Deserialize)]
struct ApiRequest {
    messages: Vec<ApiMessage>,
    #[serde(default)]
    tools: Vec<ApiTool>,
    #[serde(default)]
    stream: bool,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    seed: Option<u32>,
    chat_template_kwargs: Option<ApiKwargs>,
    tool_choice: Option<Value>,
    n: Option<u32>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Deserialize)]
struct ApiKwargs {
    enable_thinking: Option<bool>,
}

#[derive(Deserialize)]
struct ApiMessage {
    role: String,
    #[serde(default)]
    content: Value,
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ApiToolCall>,
}

#[derive(Deserialize)]
struct ApiToolCall {
    function: ApiFunction,
}

#[derive(Deserialize)]
struct ApiFunction {
    name: String,
    /// JSON text (OpenAI) or an object.
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Deserialize)]
struct ApiTool {
    function: ApiFunction,
}

/// A process-wide sequence number for ids, with the start time, so ids stay unique across requests
/// and across restarts.
fn next_id(prefix: &str) -> String {
    static START: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let start = *START.get_or_init(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    format!("{prefix}-{start:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The `usage` object OpenAI clients expect.
fn usage(stats: &GenStats) -> Value {
    json!({
        "prompt_tokens": stats.prompt_tokens,
        "completion_tokens": stats.generated,
        "total_tokens": stats.prompt_tokens + stats.generated,
    })
}

/// Everything a generation needs, owned so it can move into a blocking task.
struct Prepared {
    prompt: String,
    thinking: bool,
    schemas: HashMap<String, Value>,
    params: GenParams,
}

enum ApiError {
    BadRequest(String),
    Internal(String),
}

impl ApiError {
    fn response(self) -> Response {
        let (status, msg) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({"error": {"message": msg, "type": "invalid_request_error"}}))).into_response()
    }
}

fn content_text(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => out.push_str(p.get("text").and_then(Value::as_str).unwrap_or("")),
                    other => return Err(format!("unsupported content part type {other:?}; only text is supported")),
                }
            }
            Ok(out)
        }
        _ => Err("content must be a string, null or a list of text parts".into()),
    }
}

fn arguments_map(v: &Value) -> Result<Map<String, Value>, String> {
    match v {
        Value::Null => Ok(Map::new()),
        Value::Object(m) => Ok(m.clone()),
        Value::String(s) if s.trim().is_empty() => Ok(Map::new()),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(format!("tool arguments must be a JSON object, got {s:?}")),
        },
        _ => Err("tool arguments must be a JSON object".into()),
    }
}

fn prepare(req: &ApiRequest, prefill: PrefillSchedule) -> Result<Prepared, ApiError> {
    let bad = |m: String| ApiError::BadRequest(m);
    if req.messages.is_empty() {
        return Err(bad("messages must not be empty".into()));
    }
    if req.n.is_some_and(|n| n != 1) {
        return Err(bad("n must be 1: one generation per request".into()));
    }
    let tools_offered = match &req.tool_choice {
        None => true,
        Some(Value::String(s)) if s == "auto" => true,
        Some(Value::String(s)) if s == "none" => false,
        Some(Value::String(s)) if s == "required" => {
            return Err(bad("tool_choice \"required\" is not supported: the server cannot force a tool call".into()));
        }
        Some(Value::Object(_)) => {
            return Err(bad("a named tool_choice is not supported: the server cannot force a tool call".into()));
        }
        Some(other) => return Err(bad(format!("unsupported tool_choice {other}"))),
    };
    for key in req.extra.keys() {
        if !KNOWN_FIELDS.contains(&key.as_str()) {
            tracing::warn!(field = %key, "ignoring a request field this server does not support");
        }
    }
    let mut messages = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        let mut content = content_text(&m.content).map_err(bad)?;
        if m.role == "tool" {
            // Tool output is data: the same neutralizing as the agent applies (see plan step 1).
            content = sanitize_tool_output(&content);
        }
        let mut tool_calls = Vec::new();
        for tc in &m.tool_calls {
            tool_calls.push(ToolCall {
                name: tc.function.name.clone(),
                arguments: arguments_map(&tc.function.arguments).map_err(bad)?,
            });
        }
        messages.push(Message {
            role: m.role.clone(),
            content,
            reasoning_content: m.reasoning_content.clone(),
            tool_calls,
        });
    }
    let tools: Vec<ToolDef> = if tools_offered {
        req.tools
            .iter()
            .map(|t| ToolDef {
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                parameters: t.function.parameters.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let schemas = tools.iter().map(|t| (t.name.clone(), t.parameters.clone())).collect();
    let thinking = req.chat_template_kwargs.as_ref().and_then(|k| k.enable_thinking).unwrap_or(true);
    let prompt = chat::render(&messages, &tools, thinking).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let defaults = GenParams::default();
    let params = GenParams {
        seed: req.seed.unwrap_or(defaults.seed),
        temp: req.temperature.unwrap_or(defaults.temp),
        top_p: req.top_p.unwrap_or(defaults.top_p),
        max_tokens: req.max_tokens.unwrap_or(defaults.max_tokens),
        top_logprobs: 0,
        prefill,
        message_starts: Vec::new(),
    };
    Ok(Prepared { prompt, thinking, schemas, params })
}

fn map_engine_error(e: EngineError) -> ApiError {
    match e {
        EngineError::PromptTooLong { .. } | EngineError::EmptyPrompt => ApiError::BadRequest(e.to_string()),
        other => ApiError::Internal(other.to_string()),
    }
}

/// Runs one generation, calling `on_piece` for each parsed piece. Returns the stats.
///
/// `should_stop` is checked between events. When it says stop (the client went away), the loop
/// ends and the generation is dropped, which cancels it within a token.
fn generate(
    engine: &Engine,
    p: &Prepared,
    should_stop: impl Fn() -> bool,
    mut on_piece: impl FnMut(Piece),
) -> Result<GenStats, ApiError> {
    let tokens = engine.tokenize(&p.prompt, false, true).map_err(map_engine_error)?;
    let prompt_tokens = tokens.len();
    let mut parser = StreamParser::new(p.thinking, p.schemas.clone());
    let mut stats = None;
    for ev in engine.generate_chat(tokens, p.params.clone()).map_err(map_engine_error)? {
        if should_stop() {
            break;
        }
        match ev.map_err(map_engine_error)? {
            Event::Text(t) => parser.push(&t).into_iter().for_each(&mut on_piece),
            Event::Token { .. } => {}
            Event::Done(s) => stats = Some(s),
        }
    }
    parser.finish().into_iter().for_each(&mut on_piece);
    Ok(stats.unwrap_or(GenStats {
        finish: Finish::Cancelled,
        prompt_tokens,
        reused_tokens: 0,
        chunks: Vec::new(),
        generated: 0,
        prompt_ms: 0.0,
        gen_ms: 0.0,
    }))
}

fn finish_reason(finish: Finish, has_calls: bool) -> &'static str {
    if has_calls {
        "tool_calls"
    } else if finish == Finish::Length {
        "length"
    } else {
        "stop"
    }
}

async fn chat_completions(State(state): State<Arc<AppState>>, Json(req): Json<ApiRequest>) -> Response {
    let prepared = match prepare(&req, state.prefill) {
        Ok(p) => p,
        Err(e) => return e.response(),
    };
    if req.stream {
        stream_response(state, prepared).await
    } else {
        blocking_response(state, prepared).await
    }
}

async fn blocking_response(state: Arc<AppState>, p: Prepared) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, ApiError> {
        let engine = state.engine.lock().map_err(|_| ApiError::Internal("engine lock poisoned".into()))?;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<Value> = Vec::new();
        let mut malformed: Vec<String> = Vec::new();
        let stats = generate(&engine, &p, || false, |piece| match piece {
            Piece::Reasoning(t) => reasoning.push_str(&t),
            Piece::Content(t) => content.push_str(&t),
            Piece::ToolCall { name, arguments } => calls.push(json!({
                "id": next_id("call"),
                "type": "function",
                "function": {"name": name, "arguments": Value::Object(arguments).to_string()},
            })),
            Piece::MalformedToolCall(raw) => {
                content.push_str(&raw);
                malformed.push(raw);
            }
        })?;
        let mut message = json!({"role": "assistant", "content": content.trim()});
        if !reasoning.trim().is_empty() {
            message["reasoning_content"] = json!(reasoning.trim());
        }
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        if !malformed.is_empty() {
            // Not part of the OpenAI schema: a call the model wrote but the server could not read.
            message["malformed_tool_calls"] = json!(malformed);
        }
        Ok(json!({
            "id": next_id("chatcmpl"),
            "object": "chat.completion",
            "created": now_secs(),
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason(stats.finish, !calls.is_empty()),
            }],
            "usage": usage(&stats),
        }))
    })
    .await;
    match result {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(e)) => e.response(),
        Err(e) => ApiError::Internal(e.to_string()).response(),
    }
}

async fn stream_response(state: Arc<AppState>, p: Prepared) -> Response {
    let (tx, rx) = mpsc::channel::<Value>(64);
    tokio::task::spawn_blocking(move || {
        let send = |v: Value| {
            let _ = tx.blocking_send(v);
        };
        let id = next_id("chatcmpl");
        let created = now_secs();
        let chunk = |delta: Value, finish: Option<&str>| {
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": MODEL_ID,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            })
        };
        let engine = match state.engine.lock() {
            Ok(e) => e,
            Err(_) => {
                send(json!({"error": {"message": "engine lock poisoned"}}));
                return;
            }
        };
        let mut calls = 0usize;
        // A client that has gone away closes the channel: stop generating.
        let should_stop = || tx.is_closed();
        let result = generate(&engine, &p, should_stop, |piece| match piece {
            Piece::Reasoning(t) => send(chunk(json!({"reasoning_content": t}), None)),
            Piece::Content(t) => send(chunk(json!({"content": t}), None)),
            Piece::ToolCall { name, arguments } => {
                send(chunk(
                    json!({"tool_calls": [{
                        "index": calls,
                        "id": next_id("call"),
                        "type": "function",
                        "function": {"name": name, "arguments": Value::Object(arguments).to_string()},
                    }]}),
                    None,
                ));
                calls += 1;
            }
            Piece::MalformedToolCall(raw) => {
                send(chunk(json!({"content": raw, "malformed_tool_calls": [raw]}), None));
            }
        });
        match result {
            Ok(stats) => {
                let mut last = chunk(json!({}), Some(finish_reason(stats.finish, calls > 0)));
                last["usage"] = usage(&stats);
                send(last);
            }
            Err(ApiError::BadRequest(m)) | Err(ApiError::Internal(m)) => {
                send(json!({"error": {"message": m}}));
            }
        }
    });
    use futures::StreamExt;
    let stream = ReceiverStream::new(rx).map(|v| Ok::<_, std::convert::Infallible>(SseEvent::default().data(v.to_string())));
    let done = futures::stream::once(async { Ok::<_, std::convert::Infallible>(SseEvent::default().data("[DONE]")) });
    Sse::new(stream.chain(done)).keep_alive(KeepAlive::default()).into_response()
}

async fn models() -> Json<Value> {
    Json(json!({"object": "list", "data": [{"id": MODEL_ID, "object": "model", "owned_by": "local"}]}))
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    if !args.host.is_loopback() && !args.allow_remote {
        bail!("refusing to bind {} without --allow-remote (the endpoint has no authentication)", args.host);
    }
    let cache_mode = match args.cache_mode.as_str() {
        "per_loop" => CacheMode::PerLoop,
        _ => CacheMode::LastLoop,
    };

    verify::verify_fork()?;
    tracing::info!("checking model sha256 (a few seconds)");
    verify::verify_model(&args.model)?;

    let mut opts = EngineOptions::new(LoadOptions {
        path: args.model.clone(),
        loops: args.loops,
        cache_mode,
        n_gpu_layers: -1,
        vocab_only: false,
    });
    opts.n_ctx = args.ctx;
    opts.n_batch = args.batch;
    opts.n_ubatch = args.ubatch;
    opts.threads = args.threads;
    opts.prefix_reuse = false;
    let engine = Engine::start(opts).context("starting the engine")?;
    chat::check_template(&engine.info.chat_template.clone().unwrap_or_default())?;
    tracing::info!(loops = args.loops, ctx = args.ctx, "engine ready");

    let state = Arc::new(AppState { engine: Mutex::new(engine), prefill: args.prefill });
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/health", get(health))
        .with_state(state);

    let addr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
