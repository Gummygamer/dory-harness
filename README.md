# dory-harness

Rust harness for NVIDIA Nemotron Dory 1.0 4B, running on the llama.cpp fork at PR #30306
(https://github.com/ggml-org/llama.cpp/pull/30306, head commit `1c3cdd2`, fork at
https://github.com/brianchmielNV/llama.cpp). Build the fork first, then point `DORY_LLAMA_DIR` at it
(the default is `../../../llama.cpp-dory` relative to `crates/dory-sys`).
The plans are in `docs/`: `RUST_HARNESS_PLAN.md` and `RUST_HARNESS_FIX_PLAN.md`.

## Crates

- `dory-sys`: bindgen bindings to `include/llama.h`, linked to the fork's `libllama.so`.
- `dory-engine`: checksum and commit pins, RAII model/context, worker-thread engine, prefill schedule,
  chat renderer, streaming parser, tool allowlist, token logprobs.
- `dory-cli`: `dory info | template | run | chat | raw | bench`. `raw --chat-turn` uses the chat endpoint's
  prefill breaks.
- `dory-serve`: optional OpenAI-style `/v1/chat/completions` (streaming, tool calls returned to the
  client, one generation at a time). Binds to loopback unless `--allow-remote` is given.

## Build and run

    cargo build --release -p dory-cli -p dory-serve
    target/release/dory-cli info
    target/release/dory-cli run "What is the capital of Portugal? Answer in one word."
    target/release/dory-cli --tools examples/tools.json chat
    target/release/dory-serve --ctx 4096 --port 8080

Defaults: 2 loops, `last_loop`, 32K context, batch 2048, ubatch 512, all layers on GPU,
flash attention auto, prefix reuse off, prefill `server` (see below), temp 1.0, top-p 0.95.

Prefill: `--prefill server` (default) splits the prompt the way llama-server does, with checkpoints on.
The split changes the logits for this model, so it is part of the output. `--prefill single` decodes in
full batches. Every `raw` and `bench` result reports the chunk list it used. Startup verifies the model SHA-256 and
the fork commit (a few seconds with a warm page cache).

## Checks against llama-server

Run each script with the reference server up (see the plan's known-good command) where noted.

- `scripts/compare_reference.py`: renderer prompt and token IDs. 30/30 identical.
- `scripts/compare_generation.py collect|check`: runs 9 greedy cases (including 600- and 5000-token
  prompts). `check` compares `raw` with the server's `/completion` text, token IDs, and logprobs
  (tolerance 1e-3), and `raw --chat-turn` with the server's `/v1/chat/completions` parsed message and
  logprobs. Both reference calls use `cache_prompt: false`.
- `scripts/loop_matrix.sh`: runs the above for 1/2/4 loops and both cache modes, with an 8K reference
  context (one GPU user at a time). Set `OUT=dir` to choose the output directory.
- `scripts/compare_saved.py OLD NEW`: checks that saved harness outputs are identical, case by case.
- `scripts/gen_long_case.py`: builds a long needle case of a chosen size.
- `scripts/logit_probe.py`: first-token logprobs with optional harness flags (`collect` now sends tools).
- `scripts/cli_acceptance.sh`, `scripts/chat_acceptance.sh`: CLI and chat acceptance.
- `scripts/bench_grid.sh`: VRAM and speed grid for loops, cache mode and context.
- `scripts/check_pr.sh`: read-only check of PR #30306 state and head against the pin.

## Results

Acceptance (plan steps 7 and 8): CLI 6/6, chat deterministic and session restore pass, prefix reuse
saves tokens. Unit tests: 20 non-GPU; GPU integration tests: 5/5 (run with `--test-threads=1`, since each
loads the model).

Renderer: prompt and token IDs identical to llama-server on 30/30 cases.

Logit gap (fixed): caused by prompt chunking. `llama-server` splits a prompt at checkpoint points, and the
harness used to decode it whole. The harness now uses the server's split by default. Across the five
loop/cache configurations, all 9 cases match llama-server on the `/completion` path and on the chat path,
with token IDs identical and logprob gaps under 0.001. Before the fix, 18 of 25 matched; the cases that
differed were thinking-on near-ties with gaps up to 0.225.

Long-context: a 60,572-token needle prompt at 64K context answers correctly (2 loops, `last_loop`).

Earlier: a 25,954-token prompt answered correctly, and a 74,054-token prompt was rejected cleanly.

## Open items

- **64K context** works now. The harness sets `n_outputs_max = 1`, as llama-server does, which saves about
  220 MiB of compute buffer at every context size. 64K uses 5587 MiB of the 5806 MiB available. 128K still
  fails at KV-cache allocation.
- **Prefix reuse** is off by default: it changes how the prompt is chunked, which changed a second tool
  round's output.
- **Step 10 pin move** waits for PR #30306 to merge. The PR is still open with its head unchanged.
- The server's `/v1/chat/completions` and `/completion` replies can differ for the same prompt under default
  settings, because the chat endpoint breaks its prefill at user messages. The harness reproduces each one:
  `raw` for `/completion`, `raw --chat-turn` for the chat endpoint.
