"""Stub weather tool for testing the harness. It reads the call arguments as JSON on stdin and
prints fixed data. It does not contact any weather service."""
import json
import sys

args = json.load(sys.stdin)
city = args.get("city", "unknown")
print(json.dumps({"city": city, "forecast": "sunny", "temp_c": 21, "note": "stub data"}))
