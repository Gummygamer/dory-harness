# dory-harness

Rust harness for NVIDIA Nemotron Dory 1.0 4B, running on the llama.cpp fork at PR #30306
(https://github.com/ggml-org/llama.cpp/pull/30306, head commit `1c3cdd2`, fork at
https://github.com/brianchmielNV/llama.cpp). Build the fork first, then point `DORY_LLAMA_DIR` at it
(the default is `../../../llama.cpp-dory` relative to `crates/dory-sys`).
The plans are in `docs/`: `RUST_HARNESS_PLAN.md`, `RUST_HARNESS_FIX_PLAN.md` and `AGENT_TOOLS_PLAN.md`.

## Crates

- `dory-sys`: bindgen bindings to `include/llama.h`, linked to the fork's `libllama.so`.
- `dory-engine`: checksum and commit pins, RAII model/context, worker-thread engine, prefill schedule,
  chat renderer, streaming parser, tool allowlist, token logprobs.
- `dory-agent`: the agent loop, built-in file and command tools, permissions, the `bwrap` sandbox, compaction
  and session transcripts. It never prints; front ends render its events.
- `dory-cli`: `dory info | template | run | chat | raw | bench | agent`. `raw --chat-turn` uses the chat endpoint's
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

## Agent

`dory-cli agent` lets the model read and change files and run commands in a project folder. Reads
inside the folder are automatic. Writes and commands ask first, unless a permission mode or an
`--allow-command` rule allows them.

    target/release/dory-cli agent "What does src/parser.rs do?" --workspace .
    target/release/dory-cli agent "Fix the failing test" --workspace ../app --permission auto-edit \
        --allow-command "python3 -m unittest"
    target/release/dory-cli agent --workspace ../app          # interactive session
    target/release/dory-cli agent --resume SESSION_ID --workspace ../app

Permission modes:

| Mode | Read | Write in the project | Commands |
|---|---|---|---|
| `read-only` | allowed | refused | refused |
| `ask` (default) | allowed | asks | asks, unless `--allow-command` matches |
| `auto-edit` | allowed | allowed | asks, unless `--allow-command` matches |
| `full` | allowed | allowed | allowed, sandboxed (refused without a sandbox unless `--sandbox none`) |

Asking reads `/dev/tty`, not stdin. With no terminal, a call that needs a yes is refused, and the
model is told why. `--allow-command` matches a program and an argument prefix. A command containing
`;`, `&`, `|`, `<`, `>`, a backtick, `$(`, `${` or a newline never matches.

Sandbox: `--sandbox auto` (default) runs commands under `bwrap` when it works here. The root is
read-only, the project is writable, `/tmp` is private, `~/.ssh`, `~/.aws` and `~/.gnupg` are hidden,
the environment is cleared apart from a short allowlist plus `--env NAME`, and there is no network
unless `--network` is given. `--writable DIR` adds a writable folder, for example a cargo registry.
`--sandbox none` runs commands unconfined. Without bwrap, `auto` falls back to unconfined with a
warning, and `full` mode refuses.

Limits, stated plainly:

- The file-path checks confine the file tools only. A command can read anything the user can. Use
  `full` only on trusted input, and the sandbox limits what a command can write and reach.
- Injection is not solved. A model can still follow plain instructions in a file. Approval, the sandbox,
  no network by default and the cleared environment are the protections.
- A value containing the exact closing sequence of a parameter cannot be written reliably; the parser
  cuts it. A call that never closes its `</function>` is reported as malformed.
- The model's quality on agent tasks has not been measured yet. See the status in
  `docs/AGENT_TOOLS_PLAN.md`.
- Compaction (three stages) runs when the prompt passes 70% of the context. The summary stage is one
  extra model round. Transcripts keep the original messages, so a resumed session compacts again.
- Ctrl-C: once cancels the generation, command or approval; twice within two seconds exits.
- The approval prompt has no timeout. Answer it, or Ctrl-C it.

Sessions live in `<project>/.dory/sessions/<id>/transcript.jsonl` (one JSON line per message, round
and end reason, flushed as written). `--resume` rebuilds the conversation. A tool call whose result
was never written is marked "outcome unknown" and is not run again.

`--json` writes one event per line on stdout, schema version 1:
`session`, `reasoning`, `content`, `tool_call`, `malformed_call`, `tool_result`, `compacted`, and a final
`finished` line with `reason`, `rounds`, `tool_calls`, `prompt_tokens`, `generated_tokens` and `session`.

Exit codes: `0` answered, `1` runtime error or the model could not finish (malformed calls, empty
answers, repeated calls, a length cut that could not be kept), `3` a limit was reached (rounds, time
or context), `4` cancelled.

`dory-serve` never runs tools. It returns calls to the client and supports `tool_choice` `auto` and
`none` (`required` and named choices return 400, since nothing can force a call).

Checks: `scripts/agent_acceptance.sh` runs eight end-to-end cases on fixture projects and checks the
files and JSON events they produce. Last result: 8 of 8 on the final run (one earlier run had one
stalled case, `fix_failing_test`, which passed on re-run; the cause of the stall is not known).
`scripts/serve_openai_check.py` checks `dory-serve` with the `openai` Python package, which needs
a venv under `target/`.
