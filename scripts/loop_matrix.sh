#!/usr/bin/env bash
# Step 9: compares the harness with llama-server for each loop count and cache mode.
# For each configuration it starts llama-server with that configuration, collects greedy
# generations and token probabilities, stops the server, then runs the harness on the GPU.
# Only one of the two is on the GPU at a time (both need most of the 6 GB).
# The context is 8K so that the 5000-token case fits. Output goes to $OUT (default: ../matrix).
# Usage: [OUT=dir] scripts/loop_matrix.sh [max_tokens]
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SERVER="${LLAMA_SERVER:?set LLAMA_SERVER to the llama-server binary}"
MODEL="${RILLWRIGHT_MODEL:-${DORY_MODEL:-}}"
[[ -n "$MODEL" ]] || { echo "set RILLWRIGHT_MODEL (legacy DORY_MODEL) to the GGUF file" >&2; exit 2; }
MAXTOK="${1:-600}"
OUT="${OUT:-$HERE/../matrix}"   # kept next to the harness, on the big drive
mkdir -p "$OUT"

configs=("1 last_loop" "2 last_loop" "4 last_loop" "2 per_loop" "4 per_loop")
for cfg in "${configs[@]}"; do
  read -r loops mode <<<"$cfg"
  dir="$OUT/${loops}_${mode}"
  echo "=== loops=$loops cache=$mode"
  "$SERVER" -m "$MODEL" --alias dory --host 127.0.0.1 --port 18080 -ngl 999 --fit off -c 8192 -np 1 \
    --jinja --reasoning on --temp 1.0 --top-p 0.95 --top-k 0 --min-p 0 --repeat-penalty 1 \
    --override-kv "dory.num_loops=int:$loops,dory.recurrent_kv_cache_mode=str:$mode" \
    > "$OUT/server_${loops}_${mode}.log" 2>&1 &
  up=no
  for _ in $(seq 1 90); do
    curl -s -m 2 127.0.0.1:18080/health | grep -q ok && { up=yes; break; }
    sleep 2
  done
  if [ "$up" = yes ]; then
    python3 -I "$HERE/scripts/compare_generation.py" collect --outdir "$dir" --max-tokens "$MAXTOK" --top-logprobs 5
  else
    echo "server did not start for $loops $mode (see $OUT/server_${loops}_${mode}.log)"
  fi
  killall llama-server 2>/dev/null; sleep 4
  if [ "$up" = yes ]; then
    python3 -I "$HERE/scripts/compare_generation.py" check --outdir "$dir" --max-tokens "$MAXTOK" \
      --loops "$loops" --cache-mode "$mode" --top-logprobs 5 --ctx 8192 --dory "$HERE/target/release/rillwright"
  fi
done
echo MATRIX_DONE
