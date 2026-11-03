#!/usr/bin/env python3
"""Serves the fake Tavily endpoint (fake_tavily.py) for scripts/agent_acceptance.sh.

Usage: serve_fake_tavily.py TABLE.json QUERY_LOG.jsonl
Prints the base URL on its first line, then serves until it is killed.
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fake_tavily import FakeTavily  # noqa: E402

with open(sys.argv[1]) as f:
    table = json.load(f)
server = FakeTavily(table, sys.argv[2]).start()
print(server.base, flush=True)
while True:
    time.sleep(3600)
