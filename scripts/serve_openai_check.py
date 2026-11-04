#!/usr/bin/env python3
"""Plan step 21: a real OpenAI client completes a multi-round streaming tool conversation with
rillwright-serve, and the server recovers after a client disconnects mid-stream.

Needs the `openai` package (e.g. in a venv under target/, which is gitignored) and a running server:
  target/release/rillwright-serve --ctx 8192 --port 18181
Usage: serve_openai_check.py [BASE_URL]   (default http://127.0.0.1:18181/v1)
"""
import json
import sys
import time
import urllib.request

from openai import OpenAI

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18181/v1"
client = OpenAI(base_url=BASE, api_key="unused")
TOOLS = [{
    "type": "function",
    "function": {
        "name": "get_weather",
        "description": "Current weather for a city.",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
    },
}]
failures = []


def check(cond, msg):
    print(("ok   " if cond else "FAIL ") + msg)
    if not cond:
        failures.append(msg)


def stream_turn(messages):
    """One streamed turn. Returns (content, tool_calls by id, finish_reason, usage)."""
    stream = client.chat.completions.create(model="dory", messages=messages, tools=TOOLS,
                                            stream=True, stream_options={"include_usage": True},
                                            max_tokens=1500, temperature=0, seed=7)
    content, calls, finish, usage = "", {}, None, None
    for chunk in stream:
        if chunk.usage:
            usage = chunk.usage
        for choice in chunk.choices:
            d = choice.delta
            if d.content:
                content += d.content
            for tc in (d.tool_calls or []):
                slot = calls.setdefault(tc.index, {"id": None, "name": "", "arguments": ""})
                if tc.id:
                    slot["id"] = tc.id
                if tc.function and tc.function.name:
                    slot["name"] += tc.function.name
                if tc.function and tc.function.arguments:
                    slot["arguments"] += tc.function.arguments
            if choice.finish_reason:
                finish = choice.finish_reason
    return content, calls, finish, usage


# Round 1: the model should ask for the weather.
messages = [{"role": "user", "content": "What is the weather in Lisbon? Use the tool."}]
content, calls, finish, usage = stream_turn(messages)
check(finish == "tool_calls", f"round 1 finish_reason is tool_calls (got {finish!r})")
check(len(calls) == 1, f"round 1 has one tool call (got {len(calls)})")
call = next(iter(calls.values())) if calls else {"id": None, "name": "", "arguments": "{}"}
check(call["name"] == "get_weather", f"round 1 calls get_weather (got {call['name']!r})")
check(call["id"] is not None, "round 1 tool call has an id")
args = json.loads(call["arguments"] or "{}")
check(args.get("city", "").lower() == "lisbon", f"round 1 arguments name Lisbon (got {args})")
check(usage is not None and usage.prompt_tokens > 0 and usage.completion_tokens > 0,
      f"round 1 usage is reported (got {usage})")

# Round 2: send the call and its result back, expect a final answer that uses it.
messages.append({"role": "assistant", "content": content or None, "tool_calls": [
    {"id": call["id"], "type": "function", "function": {"name": call["name"], "arguments": call["arguments"]}}]})
messages.append({"role": "tool", "tool_call_id": call["id"], "content": json.dumps({"city": "Lisbon", "temp_c": 21, "sky": "sunny"})})
content2, calls2, finish2, usage2 = stream_turn(messages)
check(finish2 == "stop", f"round 2 finish_reason is stop (got {finish2!r})")
check(not calls2, "round 2 makes no further tool call")
check("21" in content2, f"round 2 answer uses the tool result (got {content2[:120]!r})")

# Round 3: a second, independent call in the same conversation must get a different id.
messages2 = [{"role": "user", "content": "Weather in Porto, please. Use the tool."}]
_, calls3, _, _ = stream_turn(messages2)
ids3 = [c["id"] for c in calls3.values()]
check(ids3 and ids3[0] not in (call["id"],), f"a later call gets a new id ({ids3} vs {call['id']})")

# Disconnect mid-stream, then check the server still answers promptly.
stream = client.chat.completions.create(model="dory", messages=[{"role": "user", "content": "Write a long story about rivers."}],
                                        stream=True, max_tokens=2000, temperature=0)
for i, _ in enumerate(stream):
    if i >= 3:
        break
stream.close()
time.sleep(1)
start = time.time()
with urllib.request.urlopen(BASE.replace("/v1", "") + "/health", timeout=30) as r:
    check(r.read().decode().strip() != "" and time.time() - start < 10, "server answers /health after a disconnect")
start = time.time()
quick = client.chat.completions.create(model="dory", messages=[{"role": "user", "content": "Say hi."}], max_tokens=8, temperature=0,
                                       extra_body={"chat_template_kwargs": {"enable_thinking": False}})
check(bool(quick.choices[0].message.content) and time.time() - start < 60,
      f"the next request completes after a disconnect in {time.time() - start:.1f}s")

print("\nFAILURES:" if failures else "\nall checks passed")
sys.exit(1 if failures else 0)
