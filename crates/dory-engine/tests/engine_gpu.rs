//! GPU integration tests. They load the real model, so run them one at a time:
//!
//!   cargo test -p dory-engine --test engine_gpu -- --ignored --test-threads=1

use std::time::{Duration, Instant};

use dory_engine::{CacheMode, Engine, EngineOptions, Error, Event, Finish, GenParams, LoadOptions};

const MODEL: &str = "/path/to/drive/models/dory/NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf";

fn engine(ctx: u32, prefix_reuse: bool) -> Engine {
    let mut o = EngineOptions::new(LoadOptions {
        path: MODEL.into(),
        loops: 2,
        cache_mode: CacheMode::LastLoop,
        n_gpu_layers: -1,
        vocab_only: false,
    });
    o.n_ctx = ctx;
    o.prefix_reuse = prefix_reuse;
    Engine::start(o).expect("engine should start")
}

fn prompt(e: &Engine, text: &str) -> Vec<i32> {
    e.tokenize(text, false, true).unwrap()
}

#[test]
#[ignore = "needs the GPU and the model"]
fn same_seed_gives_same_tokens() {
    let e = engine(4096, false);
    let p = prompt(&e, "<|im_start|>user\nName three European capitals.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let params = GenParams { seed: 7, temp: 1.0, top_p: 0.95, max_tokens: 40, top_logprobs: 0, ..Default::default() };
    let (a, _) = e.generate_text(p.clone(), params.clone()).unwrap();
    let (b, _) = e.generate_text(p, params).unwrap();
    assert!(!a.is_empty());
    assert_eq!(a, b, "identical seed and prompt must give identical output");
}

#[test]
#[ignore = "needs the GPU and the model"]
fn greedy_is_repeatable_across_prefix_reuse() {
    // Reusing cached KV must not change greedy output for the same prompt.
    let e = engine(4096, true);
    let p = prompt(&e, "<|im_start|>user\nSay hi.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let greedy = GenParams { seed: 0, temp: 0.0, top_p: 1.0, max_tokens: 24, top_logprobs: 0, ..Default::default() };
    let (first, s1) = e.generate_text(p.clone(), greedy.clone()).unwrap();
    let (second, s2) = e.generate_text(p.clone(), greedy).unwrap();
    assert_eq!(s1.reused_tokens, 0);
    assert_eq!(s2.reused_tokens, p.len() - 1, "the whole prompt but its last token should be reused");
    assert_eq!(first, second);
}

#[test]
#[ignore = "needs the GPU and the model"]
fn cancel_stops_after_current_token() {
    let e = engine(4096, false);
    let p = prompt(&e, "<|im_start|>user\nWrite a long essay about rivers.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let mut stream = e.generate(p, GenParams { max_tokens: 2000, ..GenParams::default() }).unwrap();
    let mut texts = 0;
    let mut finish = None;
    while let Some(ev) = stream.next() {
        match ev.unwrap() {
            Event::Token { .. } => {}
            Event::Text(_) => {
                texts += 1;
                if texts == 3 {
                    stream.cancel();
                }
            }
            Event::Done(s) => finish = Some(s),
        }
    }
    let s = finish.expect("a Done event must follow cancellation");
    assert_eq!(s.finish, Finish::Cancelled);
    assert!(s.generated < 2000, "cancellation must stop generation early, generated {}", s.generated);
}

#[test]
#[ignore = "needs the GPU and the model"]
fn prompt_longer_than_context_is_rejected() {
    let e = engine(512, false);
    let long = "word ".repeat(2000);
    let p = prompt(&e, &long);
    let err = e.generate_text(p, GenParams::default()).unwrap_err();
    assert!(matches!(err, Error::PromptTooLong { .. }), "got {err:?}");
}

#[test]
#[ignore = "needs the GPU and the model"]
fn running_out_of_context_is_a_typed_error() {
    let e = engine(512, false);
    // Fill the cache to one slot short of the limit; the next sampled token cannot be stored.
    let mut p = prompt(&e, &"the quick brown fox ".repeat(200));
    p.truncate(511);
    let params = GenParams { seed: 1, temp: 1.0, top_p: 0.95, max_tokens: 100, top_logprobs: 0, ..Default::default() };
    match e.generate_text(p, params) {
        Err(Error::ContextExhausted { pos, n_ctx }) => {
            assert_eq!(n_ctx, 512);
            assert_eq!(pos, 512);
        }
        other => panic!("expected ContextExhausted at 512/512, got {other:?}"),
    }
}

#[test]
#[ignore = "needs the GPU and the model"]
fn dropping_a_generation_frees_the_worker_for_the_next_request() {
    let e = engine(4096, false);
    let p = prompt(&e, "<|im_start|>user\nWrite a long essay about rivers.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let mut stream = e.generate(p, GenParams { max_tokens: 2000, ..GenParams::default() }).unwrap();
    let mut texts = 0;
    while let Some(ev) = stream.next() {
        if let Event::Text(_) = ev.unwrap() {
            texts += 1;
            if texts == 3 {
                break;
            }
        }
    }
    drop(stream);
    let start = Instant::now();
    let short = prompt(&e, "<|im_start|>user\nSay hi.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let (_, s) = e.generate_text(short, GenParams { max_tokens: 1, temp: 0.0, ..GenParams::default() }).unwrap();
    assert_eq!(s.generated, 1);
    assert!(start.elapsed() < Duration::from_secs(10), "next request waited {:?}", start.elapsed());
}

#[test]
#[ignore = "needs the GPU and the model"]
fn cancel_during_prefill_keeps_the_cache_consistent() {
    let e = engine(8192, true);
    let body = "The quick brown fox jumps over the lazy dog while the river runs south. ".repeat(400);
    let p = prompt(&e, &format!("<|im_start|>user\n{body}<|im_end|>\n<|im_start|>assistant\n<think></think>"));
    assert!(p.len() > 2 * 2048, "the prompt must span at least three prefill chunks, got {}", p.len());

    let stream = e.generate(p.clone(), GenParams { max_tokens: 4, ..GenParams::default() }).unwrap();
    let handle = stream.cancel_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        handle.cancel();
    });
    let mut finish = None;
    for ev in stream {
        if let Event::Done(s) = ev.unwrap() {
            finish = Some(s);
        }
    }
    canceller.join().unwrap();
    let s = finish.expect("Done after cancel");
    let decoded: usize = s.chunks.iter().sum();
    eprintln!("prefill cancel: {:?}, decoded {decoded} of {} prompt tokens", s.finish, p.len());
    if s.finish == Finish::Cancelled && decoded < p.len() {
        // Cancelled during prefill: the next request must reuse exactly the tokens that were decoded.
        let greedy = GenParams { seed: 0, temp: 0.0, top_p: 1.0, max_tokens: 1, ..GenParams::default() };
        let (_, next) = e.generate_text(p.clone(), greedy).unwrap();
        assert_eq!(next.reused_tokens, decoded, "cache holds {decoded} decoded tokens");
    } else {
        // The prompt finished before the cancel landed; nothing to check beyond finishing.
        assert_eq!(decoded, p.len());
    }
}

#[test]
#[ignore = "needs the GPU and the model"]
fn cancel_during_generation_leaves_the_prompt_cached() {
    let e = engine(4096, true);
    let p = prompt(&e, "<|im_start|>user\nWrite a long essay about rivers.<|im_end|>\n<|im_start|>assistant\n<think></think>");
    let stream = e.generate(p.clone(), GenParams { max_tokens: 2000, ..GenParams::default() }).unwrap();
    let handle = stream.cancel_handle();
    let mut texts = 0;
    for ev in stream {
        if let Event::Text(_) = ev.unwrap() {
            texts += 1;
            if texts == 3 {
                handle.cancel();
            }
        }
    }
    let greedy = GenParams { seed: 0, temp: 0.0, top_p: 1.0, max_tokens: 1, ..GenParams::default() };
    let (_, next) = e.generate_text(p.clone(), greedy).unwrap();
    assert_eq!(next.reused_tokens, p.len() - 1, "the whole prompt should still be cached");
}

#[test]
#[ignore = "needs the GPU and the model"]
fn restored_session_reuses_its_tokens_and_matches_an_uninterrupted_run() {
    let path = std::env::temp_dir().join(format!("dory-session-{}.bin", std::process::id()));
    let path_str = path.to_str().unwrap().to_string();
    let base = "<|im_start|>user\nName three European capitals.<|im_end|>\n<|im_start|>assistant\n<think></think>";
    let greedy = GenParams { seed: 0, temp: 0.0, top_p: 1.0, max_tokens: 8, top_logprobs: 0, ..Default::default() };

    // Uninterrupted: the second turn runs straight after the first, with prefix reuse on.
    let first_engine = engine(4096, true);
    let p1 = prompt(&first_engine, base);
    let (answer, first_stats) = first_engine.generate_text(p1.clone(), greedy.clone()).unwrap();
    let saved_bytes = first_engine.save_session(&path_str).unwrap();
    assert!(saved_bytes > 0);
    let mut p2 = p1.clone();
    p2.extend(prompt(&first_engine, &answer));
    p2.extend(prompt(&first_engine, "<|im_start|>user\nNow name two more.<|im_end|>\n<|im_start|>assistant\n<think></think>"));
    let (uninterrupted, u_stats) = first_engine.generate_text(p2.clone(), greedy.clone()).unwrap();
    drop(first_engine);

    // Restored: a fresh engine loads the saved cache and then runs the same second turn.
    let e = engine(4096, true);
    let cached = e.load_session(&path_str).unwrap();
    let (restored, r_stats) = e.generate_text(p2.clone(), greedy).unwrap();
    let _ = std::fs::remove_file(&path);

    // The cache holds the prompt and every generated token that was decoded.
    assert_eq!(cached, p1.len() + first_stats.generated, "load reports the saved token count");
    assert_eq!(r_stats.reused_tokens, cached.min(p2.len() - 1), "the restored cache must be used");
    assert_eq!(r_stats.reused_tokens, u_stats.reused_tokens, "restored and uninterrupted runs must reuse the same count");
    assert_eq!(restored, uninterrupted, "greedy output after a restore must match the uninterrupted run");
}
