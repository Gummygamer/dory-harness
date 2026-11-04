#!/usr/bin/env bash
# Reproduces the configuration table in RUST_HARNESS_PLAN.md: VRAM, prefill and decode speed
# for each (loops, cache mode, context). Each row is a fresh process, so each loads the model.
# A row that cannot allocate (for example 128K) is reported as FAILED, not treated as a crash.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${BIN:-$HERE/../target/release/rillwright}"
PROMPT_TOKENS="${PROMPT_TOKENS:-512}"
GEN_TOKENS="${GEN_TOKENS:-128}"

rows=(
  "1 last_loop 4096"
  "2 last_loop 4096"
  "2 per_loop 4096"
  "4 last_loop 4096"
  "4 per_loop 4096"
  "1 last_loop 32768"
  "2 last_loop 32768"
  "4 last_loop 32768"
  "2 last_loop 65536"
  "2 last_loop 131072"
)

for row in "${rows[@]}"; do
  read -r loops mode ctx <<<"$row"
  if out=$("$BIN" --loops "$loops" --cache-mode "$mode" --ctx "$ctx" --temp 0 \
      bench --prompt-tokens "$PROMPT_TOKENS" --gen-tokens "$GEN_TOKENS" 2>/tmp/dory_bench_err.txt); then
    echo "$out"
  else
    echo "loops=$loops cache=$mode ctx=$ctx FAILED: $(grep -h 'Error\|Caused' /tmp/dory_bench_err.txt | tail -1)"
  fi
done
