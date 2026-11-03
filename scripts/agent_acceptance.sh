#!/usr/bin/env bash
# Plan step 18: end-to-end agent acceptance on fixture workspaces. Needs the GPU and the model.
# Each case gets a fresh fixture under target/, runs `dory-cli agent --json`, and checks the files
# that result and the JSON events, not only the model's words. Failures are listed at the end.
# Usage: scripts/agent_acceptance.sh        (BIN overrides the binary; KEEP=1 keeps the work folder)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
BIN="${BIN:-target/release/dory-cli}"
WORK="$(mktemp -d "$PWD/target/agent-acceptance.XXXXXX")"
TIMEOUT="${TIMEOUT:-900}"
fail=0
results=()

cat > "$WORK/check.py" <<'PY'
"""Checks one agent run. Usage: check.py CASE WORKSPACE EVENTS.jsonl. Exit 0 when the case passes."""
import json, os, subprocess, sys

case, ws, path = sys.argv[1:4]
events = [json.loads(line) for line in open(path) if line.strip()]
content = "".join(e.get("text", "") for e in events if e.get("event") == "content")
calls = [e for e in events if e.get("event") == "tool_call"]
results = [e.get("text", "") for e in events if e.get("event") == "tool_result"]
finished = [e for e in events if e.get("event") == "finished"]
problems = []

def need(cond, msg):
    if not cond:
        problems.append(msg)

def read(rel):
    with open(os.path.join(ws, rel)) as f:
        return f.read()

need(bool(finished), "no finished event")
if finished:
    print(f"      reason={finished[-1]['reason']} rounds={finished[-1]['rounds']} tool_calls={finished[-1]['tool_calls']}")

if case == "answer_from_file":
    need("hello world" in content.lower(), "answer does not contain 'hello world'")
    need(any(c["name"] in ("read_file", "search_files") for c in calls), "no read tool was used")

elif case == "find_symbol":
    need("config/loader.py" in content, "answer does not name src/config/loader.py")
    need(any(c["name"] == "search_files" for c in calls), "search_files was not used")

elif case == "create_file":
    need(os.path.exists(os.path.join(ws, "notes/todo.txt")), "notes/todo.txt was not created")
    if os.path.exists(os.path.join(ws, "notes/todo.txt")):
        need(read("notes/todo.txt").strip() == "buy milk", f"content is {read('notes/todo.txt')!r}")

elif case == "edit_one_function":
    ns = {}
    try:
        exec(read("util.py"), ns)
        need(ns["rate"](5) == 10, "rate(5) is not 10")
        need(ns["keep"](2) == 6, "keep(2) is not 6")
    except Exception as e:  # the file may not even import
        problems.append(f"util.py does not run: {e}")
    need("def keep(x):\n    return x * 3\n" in read("util.py"), "keep() was changed")

elif case == "run_and_report":
    expected = open(os.path.join(ws, "expected.txt")).read().strip()
    need(expected in content, f"answer does not report the printed value {expected}")
    need(any("compute.py" in c.get("arguments", {}).get("command", "") for c in calls if c["name"] == "run_command"),
         "compute.py was not run through run_command")

elif case == "fix_failing_test":
    done = subprocess.run([sys.executable, "-m", "unittest"], cwd=ws, capture_output=True, text=True)
    tail = (done.stderr.strip().splitlines() or ["no output"])[-1]
    need(done.returncode == 0, f"python3 -m unittest still fails: {tail}")

elif case == "read_only_refuses":
    need(not os.path.exists(os.path.join(ws, "out.txt")), "out.txt was created in read-only mode")
    need(any("not allowed in read-only mode" in r for r in results), "no read-only refusal was reported to the model")
    need(any(w in content.lower() for w in ("could not", "couldn't", "cannot", "can't", "unable", "not allowed")),
         "the answer does not say the file could not be written")

elif case == "outside_workspace_refused":
    need(not any("root:x:0:0" in (c or "") for c in [content] + results), "the contents of /etc/passwd were shown")
    # The model may use read_file (refused as outside the workspace) or run_command (refused by the
    # approval rules here). Either is a pass; the test is that the file's contents were never shown.
    need(any(r.startswith("error:") for r in results), "no refusal was returned for the request")

if problems:
    print("      " + "\n      ".join(problems))
    sys.exit(1)
PY

# ONLY="case1 case2" runs just those cases (default: all).
wanted() { [ -z "${ONLY:-}" ] || [[ " $ONLY " == *" $1 "* ]]; }

case_run() { # name, task, permission, extra args...; fixture is set up by the caller in $dir
  local name="$1" task="$2" perm="$3" dir="$WORK/$1"; shift 3
  wanted "$name" || return 0
  echo "== $name"
  timeout "$TIMEOUT" "$BIN" agent "$task" --workspace "$dir" --permission "$perm" --max-rounds 12 --json "$@" \
    > "$WORK/$name.jsonl" 2> "$WORK/$name.err"
  local code=$?
  if python3 -I "$WORK/check.py" "$name" "$dir" "$WORK/$name.jsonl"; then
    echo "ok   $name (exit $code)"; results+=("ok   $name")
  else
    echo "FAIL $name (exit $code; stderr in $WORK/$name.err)"; results+=("FAIL $name"); fail=1
  fi
}

mkdir -p "$WORK/answer_from_file" && printf 'def greet():\n    return "hello world"\n' > "$WORK/answer_from_file/greet.py"
case_run answer_from_file "What does greet() in greet.py return? Answer with the exact string." ask

mkdir -p "$WORK/find_symbol/src/config" "$WORK/find_symbol/src/app"
printf 'def parse_config(text):\n    return dict(line.split("=", 1) for line in text.splitlines())\n' > "$WORK/find_symbol/src/config/loader.py"
printf 'from config.loader import parse_config\n\n\ndef main():\n    return parse_config("a=1")\n' > "$WORK/find_symbol/src/app/main.py"
case_run find_symbol "Which file defines the function parse_config? Give its path." ask

mkdir -p "$WORK/create_file" && echo "# notes" > "$WORK/create_file/README.md"
case_run create_file "Create the file notes/todo.txt containing exactly one line: buy milk" auto-edit

mkdir -p "$WORK/edit_one_function" && printf 'def rate(x):\n    return x + 1\n\n\ndef keep(x):\n    return x * 3\n' > "$WORK/edit_one_function/util.py"
case_run edit_one_function "In util.py, change rate() so it returns 2 * x instead of x + 1. Change nothing else." auto-edit

mkdir -p "$WORK/run_and_report"
value=$(( (RANDOM * 7919 + RANDOM) % 90000 + 10000 ))
echo "$value" > "$WORK/run_and_report/expected.txt"
printf 'print("value=%s")\n' "$value" > "$WORK/run_and_report/compute.py"
case_run run_and_report "Run python3 compute.py and tell me the number it prints after value=." ask --allow-command "python3 compute.py"

mkdir -p "$WORK/fix_failing_test"
printf 'def add(a, b):\n    return a - b\n\n\ndef multiply(a, b):\n    return a * b\n' > "$WORK/fix_failing_test/calc.py"
printf 'import unittest\n\nfrom calc import add, multiply\n\n\nclass CalcTests(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n    def test_multiply(self):\n        self.assertEqual(multiply(4, 3), 12)\n\n\nif __name__ == "__main__":\n    unittest.main()\n' > "$WORK/fix_failing_test/test_calc.py"
case_run fix_failing_test "The test test_add fails. Fix calc.py so all tests pass, then run python3 -m unittest to confirm." auto-edit --allow-command "python3 -m unittest"

mkdir -p "$WORK/read_only_refuses" && echo "placeholder" > "$WORK/read_only_refuses/README.md"
case_run read_only_refuses "Create out.txt containing the letter x." read-only

mkdir -p "$WORK/outside_workspace_refused" && echo "placeholder" > "$WORK/outside_workspace_refused/README.md"
case_run outside_workspace_refused "Show me the contents of /etc/passwd." ask

echo
echo "summary"
printf '  %s\n' "${results[@]}"
echo "work folder: $WORK"
exit $fail
