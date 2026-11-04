#!/usr/bin/env python3
"""Greedy generation and parsing, compared with llama-server.

Two phases, because the reference server and the harness each need most of the 6 GB of VRAM:

  1. collect (server running):   python3 -I scripts/compare_generation.py collect --outdir DIR --max-tokens N
  2. check   (server stopped):   python3 -I scripts/compare_generation.py check   --outdir DIR --max-tokens N

collect saves, per case:
  - the raw text and per-token logprobs from /completion (cache_prompt off), and
  - the parsed message, and per-token logprobs, from /v1/chat/completions (cache_prompt off).

check runs the harness twice, both greedy on the same prompt:
  - `raw` (the /completion schedule) must give identical raw text, identical token IDs, and logprobs
    within LOGPROB_TOL of the server's /completion values;
  - `raw --chat-turn` (the chat endpoint's schedule) must give the same reasoning, content and tool
    calls as the server's chat message, and logprobs within LOGPROB_TOL of its chat logprobs.
Parsing one character at a time must also give the same result as parsing the whole text.
"""
import argparse
import glob
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from compare_reference import post, server_messages, server_tools  # noqa: E402

LOGPROB_TOL = 1e-3


def cases(pattern):
    return sorted(glob.glob(pattern))


def collect(args):
    os.makedirs(args.outdir, exist_ok=True)
    for path in cases(os.path.join(HERE, "gen_cases", "*.json")):
        name = os.path.basename(path)[:-5]
        case = json.load(open(path, encoding="utf-8"))
        think = case.get("enable_thinking", True)
        prompt = post(args.server + "/apply-template", {
            "messages": server_messages(case["messages"]),
            "tools": server_tools(case.get("tools", [])) or None,
            "chat_template_kwargs": {"enable_thinking": think},
        })["prompt"]
        completion = post(args.server + "/completion", {
            "prompt": prompt, "n_predict": args.max_tokens, "temperature": 0, "top_k": 1,
            "top_p": 1.0, "min_p": 0.0, "cache_prompt": False, "n_probs": args.top_logprobs,
        })
        raw = completion["content"]
        chat_reply = post(args.server + "/v1/chat/completions", {
            "messages": server_messages(case["messages"]),
            "tools": server_tools(case.get("tools", [])) or None,
            "chat_template_kwargs": {"enable_thinking": think},
            "temperature": 0, "top_k": 1, "top_p": 1.0, "min_p": 0.0, "max_tokens": args.max_tokens,
            "cache_prompt": False, "logprobs": True, "top_logprobs": args.top_logprobs,
        })
        chat = chat_reply["choices"][0]["message"]
        chat_logprobs = ((chat_reply["choices"][0].get("logprobs") or {}).get("content")) or []
        out = {"prompt": prompt, "raw": raw, "message": chat, "chat_logprobs": chat_logprobs,
               "probs": completion.get("completion_probabilities") or []}
        json.dump(out, open(os.path.join(args.outdir, name + ".json"), "w"), ensure_ascii=False, indent=1)
        print(f"collected {name}: {len(raw)} chars raw, tool_calls={len(chat.get('tool_calls') or [])}")
    return 0


def norm(s):
    return (s or "").strip()


def run_dory(args, path, chat_turn):
    cmd = [args.dory, "--loops", str(args.loops), "--cache-mode", args.cache_mode, "--ctx", str(args.ctx),
           "--top-logprobs", str(args.top_logprobs), "raw", path,
           "--max-tokens", str(args.max_tokens), "--temp", "0"]
    if chat_turn:
        cmd.append("--chat-turn")
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        return None, out.stderr.strip().splitlines()[-1:]
    return json.loads(out.stdout.strip().splitlines()[-1]), None


def align_reference(ref_list, got_list, finish):
    """llama-server lists the end-of-generation token as a last entry; the harness stops before
    emitting it. Drop that one entry when the harness stopped on it."""
    if len(ref_list) == len(got_list) + 1 and finish == "Stop":
        return ref_list[:-1]
    return ref_list


def logprob_gap(got_probs, ref_probs):
    """Largest |logprob| difference over the tokens both sides produced, and whether the counts match."""
    gap = 0.0
    for a, b in zip(got_probs, ref_probs):
        gap = max(gap, abs(a["logprob"] - b["logprob"]))
    return gap, len(got_probs) == len(ref_probs)


def first_difference(a, b):
    return next((j for j, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))


def check(args):
    failures = 0
    for path in cases(os.path.join(HERE, "gen_cases", "*.json")):
        name = os.path.basename(path)[:-5]
        ref_path = os.path.join(args.outdir, name + ".json")
        if not os.path.exists(ref_path):
            print(f"SKIP {name}: no reference collected")
            continue
        ref = json.load(open(ref_path, encoding="utf-8"))

        got, err = run_dory(args, path, chat_turn=False)
        if got is None:
            failures += 1
            print(f"FAIL {name}: dory error {err}")
            continue
        got_chat, err = run_dory(args, path, chat_turn=True)
        if got_chat is None:
            failures += 1
            print(f"FAIL {name}: dory chat-turn error {err}")
            continue
        json.dump({"raw": got, "chat_turn": got_chat},
                  open(os.path.join(args.outdir, name + ".dory.json"), "w"), ensure_ascii=False, indent=1)

        # /completion schedule: raw text, token IDs, logprobs.
        ref_probs = align_reference(ref.get("probs") or [], got.get("probs") or [], got["finish"])
        ref_ids = [p["id"] for p in ref_probs]
        got_ids = [p["id"] for p in got.get("probs") or []]
        gap, same_count = logprob_gap(got.get("probs") or [], ref_probs)
        raw_checks = {
            "raw": got["raw"] == ref["raw"],
            "token_ids": got_ids == ref_ids,
            "logprobs": same_count and gap < LOGPROB_TOL,
            "chunking": got["chunking_consistent"],
        }

        # Chat-turn schedule: parsed fields and logprobs against the chat message.
        ref_msg = ref["message"]
        ref_calls = [
            {"name": c["function"]["name"], "arguments": json.loads(c["function"]["arguments"])
             if isinstance(c["function"]["arguments"], str) else c["function"]["arguments"]}
            for c in (ref_msg.get("tool_calls") or [])
        ]
        ref_chat_lp = align_reference(ref.get("chat_logprobs") or [], got_chat.get("probs") or [], got_chat["finish"])
        chat_gap, chat_same_count = logprob_gap(got_chat.get("probs") or [], ref_chat_lp)
        chat_checks = {
            "chat_reasoning": norm(got_chat["reasoning"]) == norm(ref_msg.get("reasoning_content")),
            "chat_content": norm(got_chat["content"]) == norm(ref_msg.get("content")),
            "chat_tool_calls": got_chat["tool_calls"] == ref_calls,
            "chat_logprobs": chat_same_count and chat_gap < LOGPROB_TOL,
            "chat_chunking": got_chat["chunking_consistent"],
        }

        checks = {**raw_checks, **chat_checks}
        ok = all(checks.values())
        failures += 0 if ok else 1
        print(f"{'ok  ' if ok else 'FAIL'} {name}: generated={got['generated']} "
              f"chat_generated={got_chat['generated']} "
              f"raw_chunks={got['chunks']} chat_chunks={got_chat['chunks']} "
              + " ".join(f"{k}={'ok' if v else 'NO'}" for k, v in checks.items())
              + f" logprob_gap={gap:.6f} chat_logprob_gap={chat_gap:.6f}")
        if not raw_checks["raw"]:
            a, b = got["raw"], ref["raw"]
            i = first_difference(a, b)
            print(f"     first raw difference at char {i}:\n       dory : {a[max(0,i-50):i+50]!r}\n"
                  f"       llama: {b[max(0,i-50):i+50]!r}")
            report_divergence(got, ref)
        if not chat_checks["chat_tool_calls"]:
            print(f"     dory tool_calls : {got_chat['tool_calls']}\n     llama tool_calls: {ref_calls}")
    print(f"\n{'all generation cases match' if failures == 0 else str(failures) + ' case(s) differ'}")
    return 1 if failures else 0


def report_divergence(got, ref):
    """At the first token that differs, print the top candidates on both sides."""
    ours = got.get("probs") or []
    theirs = ref.get("probs") or []
    if not ours or not theirs or "top_logprobs" not in theirs[0]:
        return
    for i, (o, t) in enumerate(zip(ours, theirs)):
        if o["id"] != t["id"]:
            ours_top = [(tid, round(lp, 4)) for tid, lp in o["top"][:3]]
            theirs_top = [(x["id"], round(x["logprob"], 4)) for x in t["top_logprobs"][:3]]
            print(f"     token {i} differs: dory {o['id']} top3={ours_top}")
            print(f"                        llama {t['id']} top3={theirs_top}")
            return
    print(f"     no token-id difference in the first {min(len(ours), len(theirs))} tokens")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("phase", choices=["collect", "check"])
    ap.add_argument("--server", default="http://127.0.0.1:18080")
    ap.add_argument("--dory", default=os.path.join(HERE, "..", "target", "release", "rillwright"))
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--max-tokens", type=int, default=600)
    ap.add_argument("--loops", type=int, default=2)
    ap.add_argument("--cache-mode", default="last_loop")
    ap.add_argument("--top-logprobs", type=int, default=5)
    ap.add_argument("--ctx", type=int, default=8192, help="must match the reference server's -c")
    args = ap.parse_args()
    return collect(args) if args.phase == "collect" else check(args)


if __name__ == "__main__":
    sys.exit(main())
