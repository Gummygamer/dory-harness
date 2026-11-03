//! `dory`: command-line front end for the Dory harness.

mod turn;

use std::{
    io::{BufRead, Write, stderr, stdin, stdout},
    path::PathBuf,
    process::Command,
    time::Instant,
};

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use dory_engine::{
    CacheMode, Engine, EngineOptions, Event, GenParams, LoadOptions, PrefillSchedule,
    chat::{self, Message, ToolDef},
    model::Model,
    parse::{Piece, StreamParser},
    tokenize_model,
    tools::ToolSet,
    verify,
};

const DEFAULT_MODEL: &str = "/path/to/drive/models/dory/NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf";

#[derive(Parser)]
#[command(name = "dory", version, about = "Run Nemotron Dory through the llama.cpp PR fork")]
struct Cli {
    #[command(flatten)]
    load: LoadArgs,
    #[command(flatten)]
    gen_args: GenArgs,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct LoadArgs {
    /// Path to the GGUF file (its SHA-256 must match the pin).
    #[arg(long, global = true, default_value = DEFAULT_MODEL)]
    model: PathBuf,
    /// Recurrence loops, 1 to 4.
    #[arg(long, global = true, default_value_t = 2)]
    loops: u32,
    #[arg(long, global = true, value_enum, default_value_t = CacheModeArg::LastLoop)]
    cache_mode: CacheModeArg,
    /// Context size in tokens. 128K runs out of memory on the 6 GB card.
    #[arg(long, global = true, default_value_t = 32768)]
    ctx: u32,
    #[arg(long, global = true, default_value_t = 2048)]
    batch: u32,
    #[arg(long, global = true, default_value_t = 512)]
    ubatch: u32,
    /// Layers on the GPU; -1 means all.
    #[arg(long, global = true, default_value_t = -1, allow_negative_numbers = true)]
    gpu_layers: i32,
    #[arg(long, global = true, default_value_t = 8)]
    threads: i32,
    /// Keep a full-length cache for sliding-window layers (llama-server uses off).
    #[arg(long, global = true)]
    swa_full: bool,
    /// Flash attention: auto, on or off (llama-server uses auto).
    #[arg(long, global = true, value_enum, default_value_t = FlashArg::Auto)]
    flash_attn: FlashArg,
    /// Reuse cached KV for the prompt prefix shared with the previous request. Saves time in
    /// multi-turn chat, but under last_loop it can change greedy output. Off by default.
    #[arg(long, global = true)]
    prefix_reuse: bool,
}

#[derive(Args, Clone)]
struct GenArgs {
    #[arg(long, global = true, default_value_t = 42)]
    seed: u32,
    /// 0 selects greedy decoding.
    #[arg(long, global = true, default_value_t = 1.0, allow_negative_numbers = true)]
    temp: f32,
    #[arg(long, global = true, default_value_t = 0.95)]
    top_p: f32,
    #[arg(long, global = true, default_value_t = 4096)]
    max_tokens: u32,
    /// Render the prompt with thinking off.
    #[arg(long, global = true)]
    no_think: bool,
    /// JSON array of tools; only entries with an `exec` command can run.
    #[arg(long, global = true)]
    tools: Option<PathBuf>,
    /// Do not print reasoning to stderr.
    #[arg(long, global = true)]
    quiet_reasoning: bool,
    /// Record the logprob of each token and its top-k alternatives (used by `raw`).
    #[arg(long, global = true, default_value_t = 0)]
    top_logprobs: u32,
    /// How the prompt is split into decode calls. `server` matches llama-server (default); `single`
    /// decodes it in full batches. The split changes the output, so every result records it.
    #[arg(long, global = true, default_value = "server", value_parser = parse_prefill)]
    prefill: PrefillSchedule,
}

fn parse_prefill(s: &str) -> std::result::Result<PrefillSchedule, String> {
    s.parse()
}

#[derive(Clone, Copy, ValueEnum)]
enum FlashArg {
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum CacheModeArg {
    #[value(name = "last_loop", alias = "last-loop")]
    LastLoop,
    #[value(name = "per_loop", alias = "per-loop")]
    PerLoop,
}

impl From<CacheModeArg> for CacheMode {
    fn from(c: CacheModeArg) -> Self {
        match c {
            CacheModeArg::LastLoop => CacheMode::LastLoop,
            CacheModeArg::PerLoop => CacheMode::PerLoop,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Verify the checksum and print model metadata (no GPU memory used).
    Info,
    /// Render a case file ({messages, tools?, enable_thinking?}) to the exact prompt and token IDs, as JSON.
    Template {
        case: PathBuf,
    },
    /// Answer one prompt and exit.
    Run {
        prompt: String,
        #[arg(long)]
        system: Option<String>,
    },
    /// Interactive multi-turn chat. Commands: /reset, /save PATH, /load PATH, /exit.
    Chat {
        #[arg(long)]
        system: Option<String>,
    },
    /// Generate for a case file and print the raw text plus its parsed fields, as JSON.
    /// Also checks that parsing one character at a time gives the same result.
    Raw {
        case: PathBuf,
        /// Decode with the chat-turn breaks that `/v1/chat/completions` uses, not the `/completion`
        /// schedule. Use it when comparing with the chat endpoint.
        #[arg(long)]
        chat_turn: bool,
    },
    /// Time prefill and decode for one configuration.
    Bench {
        #[arg(long, default_value_t = 512)]
        prompt_tokens: usize,
        #[arg(long, default_value_t = 128)]
        gen_tokens: u32,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    verify::verify_fork()?;
    eprintln!("checking model sha256 (about 8 s for 3.6 GB)...");
    verify::verify_model(&cli.load.model)?;

    match &cli.cmd {
        Cmd::Info => info(&cli.load),
        Cmd::Template { case } => template(&cli, case),
        Cmd::Run { prompt, system } => run(&cli, prompt, system.as_deref()),
        Cmd::Chat { system } => chat_repl(&cli, system.as_deref()),
        Cmd::Raw { case, chat_turn } => raw(&cli, case, *chat_turn),
        Cmd::Bench { prompt_tokens, gen_tokens } => bench(&cli, *prompt_tokens, *gen_tokens),
    }
}

fn engine_options(load: &LoadArgs, vocab_only: bool) -> EngineOptions {
    let mut opts = EngineOptions::new(LoadOptions {
        path: load.model.clone(),
        loops: load.loops,
        cache_mode: load.cache_mode.into(),
        n_gpu_layers: load.gpu_layers,
        vocab_only,
    });
    opts.n_ctx = load.ctx;
    opts.n_batch = load.batch;
    opts.n_ubatch = load.ubatch;
    opts.threads = load.threads;
    opts.prefix_reuse = load.prefix_reuse;
    opts.swa_full = load.swa_full;
    opts.flash_attn = match load.flash_attn {
        FlashArg::Auto => dory_engine::model::FlashAttn::Auto,
        FlashArg::On => dory_engine::model::FlashAttn::On,
        FlashArg::Off => dory_engine::model::FlashAttn::Off,
    };
    opts
}

fn start_engine(cli: &Cli) -> Result<Engine> {
    eprintln!(
        "loading: loops={} cache={} ctx={} batch={} ubatch={} gpu_layers={}",
        cli.load.loops,
        CacheMode::from(cli.load.cache_mode).as_str(),
        cli.load.ctx,
        cli.load.batch,
        cli.load.ubatch,
        cli.load.gpu_layers
    );
    let engine = Engine::start(engine_options(&cli.load, false)).context("starting the engine")?;
    let template = engine.info.chat_template.clone().unwrap_or_default();
    chat::check_template(&template)?;
    Ok(engine)
}

fn info(load: &LoadArgs) -> Result<()> {
    let model = Model::load(&LoadOptions {
        path: load.model.clone(),
        loops: load.loops,
        cache_mode: load.cache_mode.into(),
        n_gpu_layers: 0,
        vocab_only: true,
    })?;
    let template = model.chat_template().unwrap_or_default();
    chat::check_template(&template)?;
    println!("model:              {}", load.model.display());
    println!("sha256:             ok ({})", verify::PINNED_MODEL_SHA256);
    println!("fork commit:        ok ({})", verify::PINNED_FORK_COMMIT);
    println!("chat template:      ok (Dory format)");
    println!("dory.block_count:   {}", model.meta("dory.block_count").unwrap_or_default());
    println!("dory.num_loops:     {} (GGUF default)", model.meta("dory.num_loops").unwrap_or_default());
    println!(
        "dory.cache_mode:    {} (GGUF default)",
        model.meta("dory.recurrent_kv_cache_mode").unwrap_or_default()
    );
    println!(
        "loops in use:       {} ({} virtual layers = 32 + 40 x loops), cache {}",
        load.loops,
        32 + 40 * load.loops,
        load.cache_mode.as_str_name()
    );
    Ok(())
}

impl CacheModeArg {
    fn as_str_name(self) -> &'static str {
        CacheMode::from(self).as_str()
    }
}

/// A render case: `{"messages": [...], "tools": [...], "enable_thinking": bool}`.
#[derive(serde::Deserialize)]
struct RenderCase {
    messages: Vec<Message>,
    #[serde(default)]
    tools: Vec<ToolDef>,
    enable_thinking: Option<bool>,
}

fn template(cli: &Cli, case_path: &PathBuf) -> Result<()> {
    let model = Model::load(&LoadOptions {
        path: cli.load.model.clone(),
        loops: cli.load.loops,
        cache_mode: cli.load.cache_mode.into(),
        n_gpu_layers: 0,
        vocab_only: true,
    })?;
    chat::check_template(&model.chat_template().unwrap_or_default())?;
    let case: RenderCase = serde_json::from_str(&std::fs::read_to_string(case_path)?)?;
    let thinking = case.enable_thinking.unwrap_or(!cli.gen_args.no_think);
    let prompt = chat::render(&case.messages, &case.tools, thinking)?;
    let tokens = tokenize_model(&model, &prompt, false, true)?;
    let out = serde_json::json!({ "prompt": prompt, "tokens": tokens });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn gen_params(gen_args: &GenArgs) -> GenParams {
    GenParams {
        seed: gen_args.seed,
        temp: gen_args.temp,
        top_p: gen_args.top_p,
        max_tokens: gen_args.max_tokens,
        top_logprobs: gen_args.top_logprobs,
        prefill: gen_args.prefill,
        message_starts: Vec::new(),
    }
}

fn load_tools(gen_args: &GenArgs) -> Result<ToolSet> {
    Ok(match &gen_args.tools {
        Some(p) => ToolSet::load(p)?,
        None => ToolSet::default(),
    })
}

fn vram_mib() -> Option<u64> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

fn run(cli: &Cli, prompt: &str, system: Option<&str>) -> Result<()> {
    let engine = start_engine(cli)?;
    let tools = load_tools(&cli.gen_args)?;
    let mut messages = Vec::new();
    if let Some(s) = system {
        messages.push(Message::new("system", s));
    }
    messages.push(Message::new("user", prompt));
    let opts = turn::TurnOptions {
        engine: &engine,
        tools: &tools,
        thinking: !cli.gen_args.no_think,
        params: gen_params(&cli.gen_args),
        show_reasoning: !cli.gen_args.quiet_reasoning,
    };
    let report = turn::run_turn(&mut messages, &opts)?;
    println!();
    print_stats(&report);
    Ok(())
}

fn print_stats(report: &turn::TurnReport) {
    for s in &report.stats {
        let gen_tps = if s.gen_ms > 0.0 { s.generated as f64 / (s.gen_ms / 1e3) } else { 0.0 };
        let prompt_tps = if s.prompt_ms > 0.0 {
            (s.prompt_tokens - s.reused_tokens) as f64 / (s.prompt_ms / 1e3)
        } else {
            0.0
        };
        eprintln!(
            "[{:?}] prompt {} tokens ({} reused) at {:.0} tok/s, generated {} at {:.1} tok/s",
            s.finish, s.prompt_tokens, s.reused_tokens, prompt_tps, s.generated, gen_tps
        );
    }
}

fn chat_repl(cli: &Cli, system: Option<&str>) -> Result<()> {
    let engine = start_engine(cli)?;
    let tools = load_tools(&cli.gen_args)?;
    let mut messages: Vec<Message> = system.map(|s| vec![Message::new("system", s)]).unwrap_or_default();
    eprintln!("ready. /reset, /save PATH, /load PATH, /exit");
    let mut lines = stdin().lock().lines();
    loop {
        eprint!("you> ");
        let _ = stderr().flush();
        let Some(line) = lines.next() else { break };
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" {
            break;
        }
        if line == "/reset" {
            messages = system.map(|s| vec![Message::new("system", s)]).unwrap_or_default();
            engine.clear_session()?;
            eprintln!("[session cleared]");
            continue;
        }
        if let Some(path) = line.strip_prefix("/save ") {
            let bytes = engine.save_session(path.trim())?;
            std::fs::write(format!("{}.messages.json", path.trim()), serde_json::to_string_pretty(&messages)?)?;
            eprintln!("[saved {bytes} bytes of KV cache]");
            continue;
        }
        if let Some(path) = line.strip_prefix("/load ") {
            let cached = engine.load_session(path.trim())?;
            messages = serde_json::from_str(&std::fs::read_to_string(format!("{}.messages.json", path.trim()))?)?;
            eprintln!("[loaded {cached} cached tokens]");
            continue;
        }
        messages.push(Message::new("user", line));
        print!("dory> ");
        let _ = stdout().flush();
        let opts = turn::TurnOptions {
            engine: &engine,
            tools: &tools,
            thinking: !cli.gen_args.no_think,
            params: gen_params(&cli.gen_args),
            show_reasoning: !cli.gen_args.quiet_reasoning,
        };
        let report = turn::run_turn(&mut messages, &opts)?;
        println!();
        print_stats(&report);
    }
    Ok(())
}

/// Joins adjacent text pieces of the same kind so two parses can be compared.
fn merge_pieces(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for p in pieces {
        match (out.last_mut(), p) {
            (Some(Piece::Reasoning(a)), Piece::Reasoning(b)) => a.push_str(&b),
            (Some(Piece::Content(a)), Piece::Content(b)) => a.push_str(&b),
            (_, p) => out.push(p),
        }
    }
    out
}

fn raw(cli: &Cli, case_path: &PathBuf, chat_turn: bool) -> Result<()> {
    let engine = start_engine(cli)?;
    let case: RenderCase = serde_json::from_str(&std::fs::read_to_string(case_path)?)?;
    let thinking = case.enable_thinking.unwrap_or(!cli.gen_args.no_think);
    let prompt = chat::render(&case.messages, &case.tools, thinking)?;
    let tokens = engine.tokenize(&prompt, false, true)?;
    let prompt_tokens = tokens.len();
    let schemas: std::collections::HashMap<String, serde_json::Value> = case
        .tools
        .iter()
        .map(|t| (t.name.clone(), t.parameters.clone()))
        .collect();

    let mut raw = String::new();
    let mut probs = Vec::new();
    let mut stats = None;
    let params = gen_params(&cli.gen_args);
    let generation = if chat_turn {
        engine.generate_chat(tokens, params)?
    } else {
        engine.generate(tokens, params)?
    };
    for ev in generation {
        match ev? {
            Event::Text(t) => raw.push_str(&t),
            Event::Token { id, logprob, top } => probs.push(serde_json::json!({
                "id": id,
                "logprob": logprob,
                "top": top.iter().map(|(i, lp)| serde_json::json!([i, lp])).collect::<Vec<_>>(),
            })),
            Event::Done(s) => stats = Some(s),
        }
    }
    let stats = stats.ok_or_else(|| anyhow::anyhow!("generation ended without a Done event"))?;

    let mut whole = StreamParser::new(thinking, schemas.clone());
    let mut pieces = whole.push(&raw);
    pieces.extend(whole.finish());

    let mut chunked = StreamParser::new(thinking, schemas);
    let mut chunk_pieces = Vec::new();
    let mut buf = [0u8; 4];
    for ch in raw.chars() {
        chunk_pieces.extend(chunked.push(ch.encode_utf8(&mut buf)));
    }
    chunk_pieces.extend(chunked.finish());
    let chunking_consistent = merge_pieces(pieces.clone()) == merge_pieces(chunk_pieces);

    let pieces = merge_pieces(pieces);
    let mut reasoning = String::new();
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut malformed = Vec::new();
    for p in pieces {
        match p {
            Piece::Reasoning(t) => reasoning.push_str(&t),
            Piece::Content(t) => content.push_str(&t),
            Piece::ToolCall { name, arguments } => {
                tool_calls.push(serde_json::json!({ "name": name, "arguments": arguments }))
            }
            Piece::MalformedToolCall(raw) => malformed.push(raw),
        }
    }
    let out = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "finish": format!("{:?}", stats.finish),
        "generated": stats.generated,
        "raw": raw,
        "reasoning": reasoning,
        "content": content,
        "tool_calls": tool_calls,
        "malformed": malformed,
        "chunking_consistent": chunking_consistent,
        "prefill": format!("{:?}", cli.gen_args.prefill),
        "chat_turn": chat_turn,
        "chunks": stats.chunks,
        "probs": probs,
    });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn bench(cli: &Cli, prompt_tokens: usize, gen_tokens: u32) -> Result<()> {
    let engine = start_engine(cli)?;
    let vram = vram_mib();

    // Build a prompt of about `prompt_tokens` tokens from a repeated sentence.
    let unit = "The quick brown fox jumps over the lazy dog while the river runs south. ";
    let per_unit = engine.tokenize(unit, false, false)?.len().max(1);
    let reps = prompt_tokens.div_ceil(per_unit).max(1);
    let text = unit.repeat(reps);
    let mut tokens = engine.tokenize(&text, false, false)?;
    let ctx = cli.load.ctx as usize;
    if tokens.len() + gen_tokens as usize >= ctx {
        bail!("prompt ({}) plus gen tokens ({gen_tokens}) must fit in ctx {ctx}", tokens.len());
    }
    tokens.truncate(prompt_tokens.max(1));

    let mut params = gen_params(&cli.gen_args);
    params.max_tokens = gen_tokens;
    let t = Instant::now();
    let (_, stats) = engine.generate_text(tokens, params)?;
    let wall = t.elapsed().as_secs_f64();
    let prefill_tps = (stats.prompt_tokens - stats.reused_tokens) as f64 / (stats.prompt_ms / 1e3);
    let decode_tps = stats.generated as f64 / (stats.gen_ms / 1e3);
    println!(
        "loops={} cache={} ctx={} batch={} prompt={} gen={} vram_mib={} prefill={:?} chunks={} prefill_tok_s={:.0} decode_tok_s={:.1} wall_s={:.2}",
        cli.load.loops,
        CacheMode::from(cli.load.cache_mode).as_str(),
        cli.load.ctx,
        cli.load.batch,
        stats.prompt_tokens,
        stats.generated,
        vram.map(|v| v.to_string()).unwrap_or_else(|| "n/a".into()),
        cli.gen_args.prefill,
        stats.chunks.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("+"),
        prefill_tps,
        decode_tps,
        wall
    );
    let _ = stdout().flush();
    Ok(())
}
