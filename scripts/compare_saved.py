#!/usr/bin/env python3
"""Compares two directories of saved harness outputs (*.dory.json), case by case.

Used to check that a change did not alter output: run `compare_generation.py check` into a
directory, then compare it with the snapshot taken before the change.

  python3 -I scripts/compare_saved.py OLD_DIR NEW_DIR [OLD_DIR2 NEW_DIR2 ...]

Per case it reports whether the raw text, the parsed fields and the token IDs are identical, and
the largest difference in the top logprobs. Exits 1 if any case differs in text or token IDs.
"""
import glob
import json
import os
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def compare(old_path, new_path):
    old, new = load(old_path), load(new_path)
    same_text = all(old.get(k) == new.get(k) for k in ("raw", "reasoning", "content", "tool_calls", "finish"))
    old_ids = [p["id"] for p in old.get("probs") or []]
    new_ids = [p["id"] for p in new.get("probs") or []]
    same_ids = old_ids == new_ids
    max_gap = 0.0
    for a, b in zip(old.get("probs") or [], new.get("probs") or []):
        for (ta, la), (tb, lb) in zip(a.get("top") or [], b.get("top") or []):
            if ta == tb:
                max_gap = max(max_gap, abs(la - lb))
    return same_text and same_ids, same_text, same_ids, max_gap


def main(argv):
    if len(argv) < 2 or len(argv) % 2:
        print(__doc__)
        return 2
    all_ok = True
    for old_dir, new_dir in zip(argv[0::2], argv[1::2]):
        print(f"== {os.path.basename(old_dir.rstrip('/'))} -> {os.path.basename(new_dir.rstrip('/'))}")
        for old_path in sorted(glob.glob(os.path.join(old_dir, "*.dory.json"))):
            name = os.path.basename(old_path)
            new_path = os.path.join(new_dir, name)
            if not os.path.exists(new_path):
                print(f"  MISSING {name}")
                all_ok = False
                continue
            ok, same_text, same_ids, gap = compare(old_path, new_path)
            all_ok &= ok
            print(f"  {'same' if ok else 'DIFF'} {name}: text={same_text} token_ids={same_ids} "
                  f"max_logprob_gap={gap:.6f}")
    print("ALL IDENTICAL" if all_ok else "DIFFERENCES FOUND")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
