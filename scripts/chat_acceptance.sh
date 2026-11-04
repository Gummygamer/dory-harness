#!/usr/bin/env bash
# Step 8 acceptance: multi-turn tool chat, prefix reuse, and session save/restore.
# Needs the GPU and the model. Greedy decoding.
#
# Expected results:
#  - the default (no prefix reuse) run is deterministic: two runs give identical output;
#  - with --prefix-reuse, later turns skip cached prompt tokens;
#  - prefix reuse is NOT required to match the default output. It changes how the prompt is split
#    into prefill chunks, and the split changes the logits in any cache mode, so greedy output can
#    differ (measured under last_loop: "21°C" vs "21 °C" in a second tool round). This is reported,
#    not failed. Prefix reuse is off by default.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
BIN="${BIN:-target/release/rillwright}"
TMP=$(mktemp -d)
fail=0
TURNS=$'What is the weather in Lisbon?\nAnd in Porto?\n/exit\n'
CHAT=(--temp 0 --tools examples/tools.json --max-tokens 800 chat)

"$BIN" "${CHAT[@]}" >"$TMP/default1.txt" 2>"$TMP/default1_err.txt" <<<"$TURNS"
"$BIN" "${CHAT[@]}" >"$TMP/default2.txt" 2>"$TMP/default2_err.txt" <<<"$TURNS"
"$BIN" --prefix-reuse "${CHAT[@]}" >"$TMP/reuse.txt" 2>"$TMP/reuse_err.txt" <<<"$TURNS"

if cmp -s "$TMP/default1.txt" "$TMP/default2.txt"; then
  echo "ok   default run is deterministic"
else
  echo "FAIL default runs differ (see $TMP)"; fail=1
fi

reused=$(grep -o '([0-9]* reused)' "$TMP/reuse_err.txt" | tr -dc '0-9\n' | sort -n | tail -1)
if [ "${reused:-0}" -gt 0 ]; then
  echo "ok   with --prefix-reuse, later turns reuse $reused cached prompt tokens"
else
  echo "FAIL no prompt tokens were reused with --prefix-reuse"; fail=1
fi
if cmp -s "$TMP/default1.txt" "$TMP/reuse.txt"; then
  echo "info prefix reuse output identical to default this time"
else
  echo "info prefix reuse output differs from default (known under last_loop; default is off)"
fi
grep -q '\[tool call\] get_weather' "$TMP/default1_err.txt" && echo "ok   tool calls ran during chat" || { echo "FAIL no tool call in chat"; fail=1; }

# Session save and restore: the restored cache should report cached tokens and answer.
SESSION="$TMP/session"
printf 'What is the weather in Lisbon?\n/save %s\n/exit\n' "$SESSION" | "$BIN" --temp 0 --tools examples/tools.json --max-tokens 800 chat >/dev/null 2>"$TMP/save_err.txt"
printf '/load %s\nAnd in Porto?\n/exit\n' "$SESSION" | "$BIN" --temp 0 --tools examples/tools.json --max-tokens 800 chat >"$TMP/load_out.txt" 2>"$TMP/load_err.txt"
grep -q 'loaded [1-9][0-9]* cached tokens' "$TMP/load_err.txt" && echo "ok   session restored with cached tokens" || { echo "FAIL session restore (see $TMP/load_err.txt)"; fail=1; }
[ -s "$TMP/load_out.txt" ] && echo "ok   answered after restore" || { echo "FAIL no answer after restore"; fail=1; }

echo "artifacts in $TMP"
exit $fail
