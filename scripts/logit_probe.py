#!/usr/bin/env python3
"""Compares the first few token logprobs of the harness with llama-server on fixed prompts.

  collect (server running): python3 -I scripts/logit_probe.py collect --outdir DIR
  check   (server stopped): python3 -I scripts/logit_probe.py check --outdir DIR [dory flags...]

Pass harness flags after `check` (for example --swa-full --flash-attn off). Prints, per prompt,
the largest absolute difference in the chosen token's logprob and whether the token ids match.
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

N = 3


def collect(args):
    os.makedirs(args.outdir, exist_ok=True)
    for path in sorted(glob.glob(os.path.join(HERE, "gen_cases", "*.json"))):
        name = os.path.basename(path)[:-5]
        case = json.load(open(path, encoding="utf-8"))
        prompt = post(args.server + "/apply-template", {
            "messages": server_messages(case["messages"]),
            # Tools must be included: `raw` renders them, so the prompts only match if the server's do.
            "tools": server_tools(case.get("tools", [])) or None,
            "chat_template_kwargs": {"enable_thinking": case.get("enable_thinking", True)},
        })["prompt"]
        resp = post(args.server + "/completion", {
            "prompt": prompt, "n_predict": N, "temperature": 0, "top_k": 1, "top_p": 1.0,
            "min_p": 0.0, "cache_prompt": False, "n_probs": 5,
        })
        json.dump(resp["completion_probabilities"], open(os.path.join(args.outdir, name + ".json"), "w"))
        print("collected", name)
    return 0


def check(args):
    worst_all = 0.0
    for path in sorted(glob.glob(os.path.join(HERE, "gen_cases", "*.json"))):
        name = os.path.basename(path)[:-5]
        ref = json.load(open(os.path.join(args.outdir, name + ".json")))
        cmd = [args.dory] + args.dory_flags + ["raw", path, "--max-tokens", str(N), "--temp", "0", "--top-logprobs", "5"]
        out = subprocess.run(cmd, capture_output=True, text=True)
        if out.returncode != 0:
            print(f"{name}: dory error {out.stderr.strip().splitlines()[-1:]}")
            continue
        got = json.loads(out.stdout.strip().splitlines()[-1])["probs"]
        ids_ok = [g["id"] for g in got[:N]] == [r["id"] for r in ref[:N]]
        diffs = []
        for g, r in zip(got[:N], ref[:N]):
            chosen_ref = next((x["logprob"] for x in r["top_logprobs"] if x["id"] == r["id"]), None)
            diffs.append(abs(g["logprob"] - (chosen_ref if chosen_ref is not None else r["top_logprobs"][0]["logprob"])))
        worst = max(diffs) if diffs else float("nan")
        worst_all = max(worst_all, worst)
        print(f"{name}: ids_match={ids_ok} max|dlogprob|={worst:.4f} per_token={[round(d,4) for d in diffs]}")
    print(f"worst logprob difference across prompts: {worst_all:.4f}")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("phase", choices=["collect", "check"])
    ap.add_argument("--server", default="http://127.0.0.1:18080")
    ap.add_argument("--dory", default=os.path.join(HERE, "..", "target", "release", "dory-cli"))
    ap.add_argument("--outdir", required=True)
    args, rest = ap.parse_known_args()
    args.dory_flags = rest
    return collect(args) if args.phase == "collect" else check(args)


if __name__ == "__main__":
    sys.exit(main())
