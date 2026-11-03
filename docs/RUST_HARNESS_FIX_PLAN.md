# Plan: fix the logit gap and the 64K context failure

**Written:** 2026-11-02. Follows `RUST_HARNESS_PLAN.md`.

**Status (2026-11-02):** Applied. Steps 1–9 are done.
- Fix 2 (steps 6–9): `n_outputs_max = 1`. 64K loads at 5587 MiB. The 25 saved cases are byte-identical, and a 60,572-token needle prompt answers correctly.
- Fix 1 (steps 1–5): prefill schedule in `crates/dory-engine/src/prefill.rs`, `--prefill` flag, chat-turn breaks (`raw --chat-turn`, `generate_chat`). `loop_matrix.sh` at 8K context with 600- and 5000-token cases: all 9 cases match llama-server in all 5 configurations (see `src/matrix_fix1`).
- One deviation from step 3: chat user-message positions are found by matching the tokenized user-turn marker, as `find_message_spans` does in the server, rather than by having `chat::render` report them.
- Done after the plan: deleted `src/dory_scratch_build`. Posted the chunking finding to PR #30306 as a comment (https://github.com/ggml-org/llama.cpp/pull/30306#issuecomment-6591609173), marked as needing double-checking.

**Both causes are found, and both are small settings in the harness:**

1. **Logit gap:** `llama-server` splits each prompt before decoding, and the harness doesn't. Dory's outputs change with the split.
2. **64K failure:** the harness reserves output space for 512 tokens during prefill, and the server reserves it for 1.

## Fix 1: the logit gap

### What the server does

`llama-server` ends a prompt batch 4 tokens before the end of the prompt, and again at `4 + n_ubatch` tokens before the end, so it can save checkpoints (`tools/server/server-context.cpp:4503-4535`). A 275-token prompt is decoded as 271 + 4. The harness decodes it as one chunk of 275.

### Evidence

Same 5 prompts across 5 configurations (1, 2 and 4 loops with `last_loop`; 2 and 4 with `per_loop`), 4K context, greedy:

| Comparison | Identical text | Diverging cases | Worst logprob gap |
|---|---|---|---|
| Harness (single chunk) vs default server | 18/25 | 7 | 0.225 |
| Harness (single chunk) vs server with `--ctx-checkpoints 0` | 25/25 | 0 | 0.0005 |
| Harness split `n-4 + 4` vs default server | 25/25 | 0 | 0.0006 |

The server's chat endpoint also matched the harness in 25/25 cases with `--ctx-checkpoints 0` and `cache_prompt: false`.

### Why the split changes the output

The change doesn't come from later tokens leaking into earlier ones. On a true CPU run, adding later tokens to the batch changed hidden states by exactly 0 at every layer sampled (and by no more than 1e-4 relative at any layer), and the logits didn't move, for chunks large enough (see below). Two separate effects are at work:

- **Numeric amplification, in every mode.** `MUL_MAT` results shift by about one float ULP with batch size: 1.8e-7 on the GPU, 6e-8 on the CPU. Dory's squared learned scales then multiply that by about 1,000-2,500× in a single step (`node_8` to `node_13` in the node traces). After 72+ layers, logits move by up to 1-3 across the vocabulary.
- **Structural dependence, `last_loop` only.** The PR documents this. On true CPU, a 137/138 split is exact until `kqv_out-16`, the first recurrent layer, and differs from there on.

Chunk sizes measured on one prompt (275 tokens, 1 loop, `per_loop`):

| Device | Result |
|---|---|
| GPU | Every split tested differs from whole-prompt |
| True CPU | Exact when both chunks have at least 64 tokens; differs when either has 48 or fewer |

So no schedule is "the correct one", since generation itself decodes single tokens. A schedule only makes results **reproducible and comparable**. Exact agreement with another run also requires the same device, as true-CPU and GPU whole-prompt logits differ by about 1.2.

### Decision

Make the server's schedule the default, because `llama-server` is the plan's reference. Keep single-chunk as an option. There is no evidence that either schedule gives better answers.

### Steps

**1. Add a prefill schedule to the engine.**
- Add `PrefillSchedule::{Server, Single}` and a pure function that returns the chunk sizes, mirroring the server loop: at most `n_batch` tokens per chunk, breaking when the remaining tokens equal `min(n_batch, 4 + n_ubatch)` or `min(n_batch, 4)`.
- Use it in place of `chunks(n_batch)` in `engine.rs`.
- Add unit tests for `n=5 → [1, 4]`, `n=28 → [24, 4]`, `n=275 → [271, 4]`, `n=1000 → [484, 512, 4]`, `n=5000 → [2048, 2048, 388, 512, 4]`.
- **Done when:** the unit tests pass. The longer schedules are derived from reading the code and are checked against the server in step 4.

**2. Expose and record it.**
- Add `--prefill server|single` to `dory-cli` and `dory-serve`, defaulting to `server`.
- Include the schedule in `raw` output and `bench` lines, so every result says how it was produced.
- **Done when:** `raw` reports the chunk list it actually used.

**3. Match the chat endpoint.**
- The server also breaks at the last user message when it has message spans (`server-context.cpp:4503`), which `/completion` doesn't have. This is the likely reason the two endpoints disagreed, but it hasn't been tested on its own.
- Read how `last_user_message_pos` is computed, then have `chat::render` report the token position.
- Add it as a further break for `run`, `chat` and `dory-serve`, which are chat turns. `raw` keeps the `/completion` schedule.
- **Done when:** the harness matches `/v1/chat/completions` with default checkpoints and `cache_prompt: false`.

**4. Extend the comparison suite.**
- Send `cache_prompt: false` on chat requests in `compare_generation.py`. The saved chat references were collected with prompt caching on, so they can reuse the previous request's KV.
- Compare parsing against the chat message again, in place of the workaround that splits the raw text.
- Add token-level checks: no diverging token IDs, and a logprob gap under 1e-3.
- Add prompts of about 600 and 5,000 tokens, which exercise the multi-chunk schedules. None of these has been tested yet.
- **Done when:** `loop_matrix.sh` passes in all 5 configurations, including the longer prompts.

**5. Correct the tools and docs.**
- Fix `logit_probe.py`, whose `collect` omits `tools` (see corrections below).
- Change the `chat_acceptance.sh` comment that blames `last_loop` alone. Prefix reuse changes the chunking, so it would be expected to change output in any mode.
- Keep prefix reuse off by default.
- **Done when:** the README and scripts match this plan.

## Fix 2: the 64K context failure

### Cause

The library's `n_outputs_max` default of 0 means `n_batch`, so prefill reserves output logits for `min(n_ubatch, 2048) = 512` tokens across a 131,072-word vocabulary. `llama-server` sets it to 1 (`server_output_limits`, `server-context.cpp:47`).

The harness's compute buffer is therefore 223 MiB larger at every context size. At 64K that gives 118.52 + 223 = 341.52 MiB, which is exactly the allocation that failed. My earlier `--batch 512` test couldn't show this, because it still left the limit at 512.

### Evidence

VRAM by `nvidia-smi`, 2 loops, `last_loop`, from a 28 MiB idle baseline:

| Context | Harness now | Harness with `n_outputs_max=1` (scratch) | `llama-server` |
|---|---|---|---|
| 4K | 3589 MiB | 3368 MiB | 3414 MiB |
| 32K | 4625 MiB | 4404 MiB | 4450 MiB |
| 64K | out of memory | 5588 MiB | 5634 MiB |

In the scratch build, 64K generated at 48 tokens/s on a 481-token prompt, and the setting didn't change the output of the one case compared (`g04`, 2 loops).

### Steps

**6. Set `cp.n_outputs_max = 1`** in `Context::new`, with a comment explaining why. The harness only ever reads logits for the last token.
- **Done when:** `bench_grid.sh` shows 64K loading, with VRAM within about 50 MiB of the table.

**7. Confirm output is unchanged.**
- Rerun the 25 matrix cases and compare raw text and logprobs with the saved harness outputs in `src/matrix/`, before applying the Fix 1 steps.
- **Done when:** all 25 are identical.

**8. Test 64K under real load.**
- Run a prompt of about 60K tokens and generate from it. Only about 218 MiB of VRAM stays free at 64K, and before this fix an earlier 64K attempt at a smaller ubatch hit an out-of-memory error while building a CUDA graph.
- If it fails, find the largest context that survives a full prompt and make that the documented limit.
- **Done when:** a full-context run finishes, or the real limit is documented. 32K stays the default either way.

**9. Update the VRAM table** in `RUST_HARNESS_PLAN.md` and the README. Its 4K figures need replacing too.

## Order

Do Fix 2 first (steps 6-9), since it's one line and shouldn't change output. Then Fix 1 (steps 1-5), so that any output change can be attributed to the schedule.

## Corrections to what I reported earlier

- **"0.11-0.13 nats on long prompts" was wrong.** It came from a bug in my probe, which compared the harness's prompt with tools against a server prompt without them. The real first-token gap is 0.00001-0.05, growing to 0.225 during generation.
- **"Gap specific to longer prompts" was wrong** for the same reason. It's about chunking.
- **"Ruled out batch size" was wrong.** `--batch` and `--ubatch` both change chunking when the prompt is long enough, and the earlier test on `g01` (28 tokens) couldn't show it.
- **"Agrees within 16 MiB at 4K" was wrong.** That measurement read 3573 MiB for the server; with a clean baseline it reads 3414 MiB. The real gap was about 175 MiB.
- **"The endpoints are not self-consistent" was misleading.** They differ in prompt splitting and caching, and they agree with checkpoints and caching off.
- **My first "CPU" runs still used the GPU.** With `--gpu-layers 0`, `op_offload` still sends operations to the GPU. The true-CPU results here use no devices and no offload.

## Not known

- **The CPU threshold.** Why chunks of 64+ tokens are exact and chunks of 48 or fewer aren't. The `llamafile_sgemm` path at `ggml-cpu.c:1311` is one candidate; I didn't confirm it.
- **The GPU source of the ULP differences.** I didn't identify which kernel behaviour causes them.
- **The remaining 0.0006 logprob gap** after matching schedules. It's small and hasn't caused any divergence, but it isn't explained.
- **The 46 MiB VRAM gap** between the scratch harness and the server, with the harness lower.
- **Whether any of this affects answer quality.** Greedy output changes wording at near-ties, and I measured no quality difference in either direction.
- **How general the traces are.** The node-level traces used one prompt at 1 loop.

## Decision for you

The PR says chunk size matters for `last_loop`. These measurements show it also matters for `per_loop` through numeric amplification, which may be worth telling the PR author. I haven't posted anything; that's your call.

## Evidence and tools

- `src/dory-harness/investigation/`: logs of each experiment, plus `tools/layerdiff.rs` and `tools/nodediff.rs`.
- `src/matrix/` and `src/matrix_nockpt/`: saved reference and harness outputs.
- `src/dory_scratch_build/`: the scratch copy with the experiment switches (263 MB). The real harness has none of them, and this can be deleted once step 6 is done.
