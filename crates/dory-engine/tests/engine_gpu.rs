//! GPU integration tests. They load the real model, so run them one at a time:
//!
//!   cargo test -p dory-engine --test engine_gpu -- --ignored --test-threads=1

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
