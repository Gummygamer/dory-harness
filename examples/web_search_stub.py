"""Stub web search for the plan's step 1 prototype (docs/WEB_SEARCH_PLAN.md).

Reads the call arguments {"query": ..., "max_results": ...} as JSON on stdin and answers from a
fixture file. It makes no network request. The fixture defaults to web_fixture.json next to this
script; WEB_STUB_FIXTURE overrides it.
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.environ.get("WEB_STUB_FIXTURE", os.path.join(HERE, "web_fixture.json"))


def words(text):
    return {w for w in re.findall(r"[a-z0-9_]+", text.lower()) if len(w) > 2}


args = json.load(sys.stdin)
query = str(args.get("query", "")).strip()
try:
    n = max(1, min(10, int(args.get("max_results") or 5)))
except (TypeError, ValueError):
    n = 5

with open(FIXTURE, encoding="utf-8") as f:
    pages = json.load(f)

wanted = words(query)
scored = []
for page in pages:
    haystack = words(" ".join(page["keywords"]) + " " + page["title"] + " " + page["content"])
    score = len(wanted & haystack)
    if score:
        scored.append((score, page))
scored.sort(key=lambda item: -item[0])

hits = [page for _, page in scored[:n]]
if not hits:
    print(f'No results for "{query}".')
else:
    for i, page in enumerate(hits, 1):
        print(f"{i}. {page['title']}\n   {page['url']}\n   {page['content']}")
