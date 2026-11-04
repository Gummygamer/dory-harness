#!/usr/bin/env bash
# Step 7 acceptance: the Lisbon, thinking-off and weather tests through the CLI.
# Needs the GPU and the model. Run from anywhere; it changes into the harness directory.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
BIN="${BIN:-target/release/rillwright}"
fail=0
check() { # name, condition-result, detail
  if [ "$2" = yes ]; then echo "ok   $1"; else echo "FAIL $1: $3"; fail=1; fi
}

Q="What is the capital of Portugal? Answer in one word."

out=$("$BIN" run "$Q" --max-tokens 1500 2>/tmp/dory_accept_err.txt)
check "Lisbon (thinking on)" "$(grep -qi lisbon <<<"$out" && echo yes || echo no)" "stdout was: $out"
check "reasoning goes to stderr, not stdout" "$(grep -q '<think>' <<<"$out" && echo no || echo yes)" "tags leaked to stdout"

out=$("$BIN" run "$Q" --no-think --max-tokens 1500 2>/tmp/dory_accept_err2.txt)
check "Lisbon (thinking off)" "$(grep -qi lisbon <<<"$out" && echo yes || echo no)" "stdout was: $out"
check "thinking off produces no reasoning" "$(grep -q 'The user' /tmp/dory_accept_err2.txt && echo no || echo yes)" "reasoning present"

out=$("$BIN" run "What's the weather in Lisbon? Use the tool." --tools examples/tools.json --max-tokens 1500 2>/tmp/dory_accept_err3.txt)
calls=$(grep -F '[tool call] get_weather' /tmp/dory_accept_err3.txt)
check "weather tool called with city Lisbon" "$(grep -qE '"city": ?"Lisbon"' <<<"$calls" && echo yes || echo no)" "calls were: $calls"
check "final answer uses the tool result" "$(grep -qi 'sunny\|21' <<<"$out" && echo yes || echo no)" "stdout was: $out"

exit $fail
