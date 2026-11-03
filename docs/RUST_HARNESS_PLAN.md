# Plan: a Rust harness for Dory on the llama.cpp PR

**Written:** 2026-11-02

**Status (2026-11-02):** Implemented. Steps 1–9 are done, and the optional server from step 10 exists as `dory-serve`. Only the pin move in step 10 is open: [PR #30306](https://github.com/ggml-org/llama.cpp/pull/30306) is still open, with its head unchanged at `1c3cdd2`. The workspace has four crates (`dory-sys`, `dory-engine`, `dory-cli`, `dory-serve`), not the three listed in step 1. Results, checks and open items are in the top-level `README.md`; the later logit-gap and 64K fixes are in `RUST_HARNESS_FIX_PLAN.md`. The rest of this file is the original plan, kept as written.

**The PR solves it, so we don't need a native runtime.** Upstream llama.cpp still has no Dory support, because [PR #30306](https://github.com/ggml-org/llama.cpp/pull/30306) is open and unmerged. The PR head (`1c3cdd23787750814aa21a070dc4d6dc451af871`) was built with CUDA and runs the Q5_K_M file entirely on the RTX 3060. When this plan was written no Rust code existed; the harness in this repo is the result.

The final test used 2 loops, `last_loop` and a 32K context, and everything passed:

- **Reasoning:** it answered correctly at about 42 tokens/s, with reasoning and answer separated.
- **Thinking off:** this works too.
- **Tool calls:** `get_weather` was called with `{"city":"Lisbon"}`.
- **Oversized prompts:** these get a clean 400 error.

## Measured on this machine

Hardware: RTX 3060 Laptop (6 GB), i7-11800H, 22 GB RAM.

| Loops | Cache mode | Context | VRAM used | Generation (tokens/s) | Prompt (tokens/s) |
|---|---|---|---|---|---|
| 1 | `last_loop` | 4K | 3.37 GB | 56 | 2175 |
| 2 | `last_loop` | 4K | 3.37 GB | 41 | 1461 |
| 2 | `per_loop` | 4K | 3.54 GB | 31 | 1130 |
| 4 | `last_loop` | 4K | 3.37 GB | 21 | 773 |
| 4 | `per_loop` | 4K | 3.88 GB | 19 | 670 |
| **2** | **`last_loop`** | **32K** | **4.40 GB** | **30–42** | **1185** |
| 2 | `last_loop` | 64K | 5.59 GB | 31 | 1100 |
| 2 | `last_loop` | 128K | out of memory (KV cache, 4.6 GB buffer) | – | – |

VRAM figures are MiB ÷ 1000, measured with `scripts/bench_grid.sh` after `n_outputs_max = 1` (see `RUST_HARNESS_FIX_PLAN.md`). The previous figures were 3.40 / 3.41 / 3.58 / 3.44 / 3.95 / 4.45 / 5.63 GB; the reduction is about 220 MiB at each context size (221 MiB at both 4K and 32K in the scratch measurements). Speed columns were not re-measured for this change. Treat speed differences as rough: the same 32K setup gave 30 tokens/s on a 2,784-token prompt and 42 on a short one, and the cause wasn't isolated.

## Decisions

- **Defaults:** 2 loops, `last_loop`, 32K context, all layers on the GPU, temperature 1.0, top-p 0.95. 64K fits and was confirmed with a 60,572-token needle prompt, but leaves only about 0.2 GB of VRAM free (5,587 of 5,806 MiB).
- **Integration:** call `libllama`'s C API directly from Rust, which gives the harness control over loops, cache and streaming. If only an OpenAI-style endpoint is needed, `llama-server` already does that without any Rust.
- **Bindings:** generate our own with `bindgen`, since existing crates that vendor upstream llama.cpp won't include Dory until the PR merges.
- **Test reference:** use `llama-server` as the correct answer to compare the harness against.

## Limits the harness must handle

- **No context shifting:** the PR doesn't support it, and generation stopped at exactly 4096 tokens in the 4K runs. The harness has to budget tokens and fail clearly.
- **Reasoning uses a lot of tokens:** a trivial question took 788 tokens, and the long-prompt runs used up their whole budget without answering. A low `max_tokens` can return empty content.
- **`last_loop` trade-off:** the PR says it changes predictions, and that prefill chunk size matters. Batch size therefore has to be fixed and recorded.
- **The Ollama `Modelfile`** (kept outside this repo): its `num_ctx 262144` can't fit, since 128K already runs out of memory. Ollama 0.23.2 also shows no sign of Dory support, though this was only checked by searching its binary, not by trying to load the model.

## Steps

### 1. Pin the dependency

- Keep the fork at `/path/to/drive/src/llama.cpp-dory`.
- Create the workspace in `src/dory-harness` on the same drive, with `CARGO_TARGET_DIR` there too, because the root disk has only 4 GB free.
- Create three crates: `dory-sys`, `dory-engine` and `dory-cli`.
- Have startup check the model's SHA-256 (`96080be49ffd67b916018b7bdef8c484762576fb52780e10ab98ac4978a660e0`) and the fork commit.
- **Done when:** `cargo build` works from clean, and a bad checksum or commit gives a clear error.

### 2. Write `dory-sys`

- Generate bindings from `include/llama.h`, link `libllama`, and set the rpath in `build.rs`.
- Add a test that the `llama_model_kv_override` layout and the default params match the header.
- **Done when:** a Rust binary prints the model's metadata, such as 72 blocks and `dory.num_loops`.

### 3. Load the model safely

- Wrap the model, context, sampler and batch in RAII types, freeing them in the right order.
- Pass `dory.num_loops` and `dory.recurrent_kv_cache_mode` through `kv_overrides`. This also avoids the CLI's repeated-flag warning.
- Reject loop counts outside 1–4, and route `llama_log_set` into `tracing`.
- **Done when:** loops 1, 2 and 4 load, VRAM matches the table, and a 128K context returns an error rather than crashing.

### 4. Build the generation loop

- Run `llama_decode` on one dedicated thread, with channels to talk to it, because the raw pointers aren't thread-safe.
- Sampler chain: top-p, then temperature, then seeded `dist`.
- Stop on end-of-generation tokens (`<|im_end|>` is 11), buffer split UTF-8 pieces, and check the context budget before each decode.
- Add cancellation and timing stats.
- **Done when:** the same seed gives the same tokens twice, and running out of context ends with a typed error.

### 5. Render the chat template

- Read the template from the GGUF and render it with `minijinja`, which is an untested fit for this template. It uses `|items`, `loop.previtem`, `namespace` and Python-style `.split()`.
- If `minijinja` can't render it, write a small Rust renderer for this one format, backed by the same comparison tests.
- Tokenize with special-token parsing on. The GGUF sets `add_bos=false`, so don't add one.
- **Done when:** prompts and token IDs match `llama-server` exactly across cases covering system messages, history, tools and thinking on and off.

### 6. Parse the output as it streams

- **Start state:** with thinking on, the prompt already ends in `<think>\n`, so the parser must begin inside reasoning.
- **Events:** `Reasoning`, `Content`, `ToolCall` and `Done`.
- **Tool format:** parse `<tool_call><function=…><parameter=…>`, and convert parameter strings to JSON types using the tool's schema.
- **Done when:** results match `llama-server`, including when tags are split across tokens or the output is cut off.

### 7. Build the CLI

- `dory run` handles one prompt and `dory chat` is interactive, with reasoning shown separately.
- Flags: `--loops`, `--cache-mode`, `--ctx`, `--batch`, `--seed`, `--max-tokens`, `--no-think`, `--tools file.json`.
- Add `dory bench` to reproduce the table above.
- **Done when:** the Lisbon, thinking-off and weather tests all pass through the CLI.

### 8. Run the tool loop and reuse the prefix

- Add tool execution behind an allowlist, feeding results back as `tool` messages.
- Reuse the shared prefix with `llama_memory_seq_rm`, and add session save and restore.
- Test prefix reuse carefully under `last_loop`, where older tokens keep only their final-loop KV entries. If it changes output, re-evaluate the prompt instead.
- **Done when:** a multi-turn tool chat works, and later turns process only the new tokens.

### 9. Test against the reference

- Compare greedy output and top logprobs with `llama-server` for 1, 2 and 4 loops in both cache modes, and for several batch sizes.
- Add a 30K-token prompt test, because the PR says long-context testing is incomplete.
- Add a nightly check for whether the PR has merged or its head has moved.
- **Done when:** the comparison suite passes, and any difference between batch sizes is documented.

### 10. Add an optional server and move to upstream

- If an API is wanted, add `dory-serve` with `axum` for `/v1/chat/completions` and SSE. Bind it to localhost, one request at a time.
- Once the PR merges, change the pin and rerun step 9. The fork's interface may still change before then, which is the main risk of building on it.

## Reference: known-good command

This is the `llama-server` configuration behind the final test, and it can serve as the reference in steps 5, 6 and 9.

```bash
M=/path/to/drive
$M/src/llama.cpp-dory/build/bin/llama-server \
  -m $M/models/dory/NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf \
  --alias dory --host 127.0.0.1 --port 18080 \
  -ngl 999 --fit off -c 32768 -np 1 --jinja --reasoning on \
  --temp 1.0 --top-p 0.95 --top-k 0 --min-p 0 --repeat-penalty 1 \
  --override-kv dory.num_loops=int:2,dory.recurrent_kv_cache_mode=str:last_loop
```

## Not tested yet

- Partial GPU offload.

Both other items on the original list have since been covered. Contexts filled past 32K: a 60,572-token needle prompt at 64K context answered correctly, and an earlier 25,954-token prompt did too. Outputs at 1 and 4 loops: `scripts/loop_matrix.sh` compares 9 cases against `llama-server` in five configurations (1, 2 and 4 loops with `last_loop`; 2 and 4 loops with `per_loop`), with identical token IDs (see `README.md`).

## Sources

- [NVIDIA Dory 1.0 4B GGUF model card](https://huggingface.co/nvidia/NVIDIA-Nemotron-Labs-Dory1.0-4B-GGUF)
- [llama.cpp PR #30306](https://github.com/ggml-org/llama.cpp/pull/30306)
- [PR fork](https://github.com/brianchmielNV/llama.cpp)
