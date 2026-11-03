#!/usr/bin/env python3
"""Compares the Rust renderer and tokenizer with llama-server (the PR build) on every case.

Start the reference server first (see RUST_HARNESS_PLAN.md, "known-good command"), then:

    python3 -I scripts/compare_reference.py --server http://127.0.0.1:18080 --dory target/release/dory-cli

For each case file it checks that the prompt text and the token IDs are identical. Exit status
is non-zero if any case differs. Run with -I so that nothing in the working directory shadows
the standard library.
"""
import argparse
import glob
import json
import os
import subprocess
import sys
import urllib.request


def post(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())


def server_messages(messages):
    """Converts harness messages to the OpenAI-style shape llama-server expects."""
    out = []
    for m in messages:
        item = {"role": m["role"], "content": m.get("content", "")}
        if m.get("reasoning_content"):
            item["reasoning_content"] = m["reasoning_content"]
        if m.get("tool_calls"):
            item["content"] = m.get("content", "") or None
            item["tool_calls"] = [
                {"id": f"call_{i}", "type": "function",
                 "function": {"name": tc["name"], "arguments": json.dumps(tc.get("arguments", {}))}}
                for i, tc in enumerate(m["tool_calls"])
            ]
        out.append(item)
    return out


def server_tools(tools):
    return [{"type": "function", "function": t} for t in tools]


def first_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="http://127.0.0.1:18080")
    ap.add_argument("--dory", default=os.path.join(os.path.dirname(__file__), "..", "target", "release", "dory-cli"))
    ap.add_argument("--cases", default=os.path.join(os.path.dirname(__file__), "cases", "*.json"))
    args = ap.parse_args()

    failures = 0
    cases = sorted(glob.glob(args.cases))
    for path in cases:
        case = json.load(open(path, encoding="utf-8"))
        think = case.get("enable_thinking", True)
        name = os.path.basename(path)

        ref = post(args.server + "/apply-template", {
            "messages": server_messages(case["messages"]),
            "tools": server_tools(case.get("tools", [])) or None,
            "chat_template_kwargs": {"enable_thinking": think},
        })
        ref_prompt = ref["prompt"]
        ref_tokens = post(args.server + "/tokenize", {
            "content": ref_prompt, "add_special": False, "parse_special": True,
        })["tokens"]

        out = subprocess.run([args.dory, "template", path], capture_output=True, text=True)
        if out.returncode != 0:
            failures += 1
            print(f"FAIL {name}: dory-cli error: {out.stderr.strip().splitlines()[-1:]}")
            continue
        ours = json.loads(out.stdout)

        prompt_ok = ours["prompt"] == ref_prompt
        tokens_ok = ours["tokens"] == ref_tokens
        if prompt_ok and tokens_ok:
            print(f"ok   {name} ({len(ref_tokens)} tokens)")
            continue
        failures += 1
        print(f"FAIL {name}: prompt_match={prompt_ok} tokens_match={tokens_ok}")
        if not prompt_ok:
            i = first_diff(ours["prompt"], ref_prompt)
            print(f"     at char {i}:\n       dory : {ours['prompt'][max(0, i-40):i+60]!r}\n       llama: {ref_prompt[max(0, i-40):i+60]!r}")

    print(f"\n{len(cases) - failures}/{len(cases)} cases identical")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
