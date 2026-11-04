#!/usr/bin/env python3
"""Writes a long-context needle case for `rillwright raw`.

A document of numbered ledger lines (filler) holds one needle line in the middle. The final user
message asks for the needle's code, so a correct answer needs the whole prompt to be attended to.
`raw` reports prompt_tokens, which shows the real size.

  python3 -I scripts/gen_long_case.py --lines 1750 --out CASE.json
  rillwright --ctx 65536 raw CASE.json --max-tokens 64 --temp 0

About 35 tokens per filler line. Thinking is off unless --think is given.
"""
import argparse
import json

NEEDLE_CODE = "7319-KESTREL"


def filler(i):
    return (f"Ledger entry {i:05d}: shipment {(i * 7) % 997} from the Porto depot cleared customs on "
            f"day {i % 365} with no discrepancies recorded against the manifest.")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lines", type=int, required=True, help="filler lines; about 30 tokens each")
    ap.add_argument("--out", required=True)
    ap.add_argument("--think", action="store_true", help="enable thinking in the case")
    args = ap.parse_args()

    mid = args.lines // 2
    lines = []
    for i in range(args.lines):
        if i == mid:
            lines.append(f"Ledger entry NEEDLE: the access code for the Lisbon vault is {NEEDLE_CODE}.")
        lines.append(filler(i))
    document = "\n".join(lines)
    case = {
        "messages": [{
            "role": "user",
            "content": "Below is a long ledger. Somewhere in it is the access code for the Lisbon vault.\n\n"
                       + document
                       + "\n\nWhat is the access code for the Lisbon vault? Reply with the code only.",
        }],
        "enable_thinking": args.think,
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(case, f, ensure_ascii=False, indent=1)
    print(f"wrote {args.out}: {args.lines} filler lines, needle code {NEEDLE_CODE}")


if __name__ == "__main__":
    main()
