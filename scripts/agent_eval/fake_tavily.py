"""A fake of Tavily's search endpoint for the evaluation (docs/WEB_SEARCH_PLAN.md, step 10).

It runs on a free loopback port, answers `POST /search` in Tavily's response format from a
per-task table, and logs every request (query, count, whether the bearer token was right) to a
JSONL file the checker reads. It spends no credits and changes nothing between runs. The agent
reaches it through `DORY_WEB_BASE_URL`, which only debug builds read.

Table format (a dict):
  "default":        results for a query that no rule matches (may be empty)
  "rules":          [{"contains": [words], "results": [...]}, ...]; the first rule whose words all
                    appear in the lowercased query wins
  "off_topic_first": if true, the first request gets "off_topic" results, later ones the table
  "off_topic":      results for that first request
A result is {"title", "url", "content"}.
"""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FAKE_KEY = "tvly-fake-eval-key-0000"


class FakeTavily:
    def __init__(self, table, log_path):
        self.table = table
        self.log_path = log_path
        self.lock = threading.Lock()
        self.count = 0
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def start(self):
        self.thread.start()
        return self

    def stop(self):
        self.server.shutdown()
        self.server.server_close()

    def _results_for(self, query):
        q = query.lower()
        with self.lock:
            self.count += 1
            first = self.count == 1
        if first and self.table.get("off_topic_first"):
            return self.table.get("off_topic", [])
        for rule in self.table.get("rules", []):
            if all(word.lower() in q for word in rule["contains"]):
                return rule["results"]
        return self.table.get("default", [])

    def _log(self, entry):
        with self.lock, open(self.log_path, "a") as f:
            f.write(json.dumps(entry) + "\n")

    def _handler(self):
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, fmt, *args):  # keep the evaluation output quiet
                pass

            def _reply(self, status, body):
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0") or 0)
                raw = self.rfile.read(length) if length else b""
                auth_ok = self.headers.get("Authorization") == f"Bearer {FAKE_KEY}"
                try:
                    body = json.loads(raw or b"{}")
                except json.JSONDecodeError:
                    body = {}
                if self.path != "/search":
                    fake._log({"path": self.path, "auth_ok": auth_ok, "query": None})
                    return self._reply(404, {"detail": "not found"})
                query = str(body.get("query", ""))
                fake._log({"path": "/search", "auth_ok": auth_ok, "query": query, "max_results": body.get("max_results")})
                if not auth_ok:
                    return self._reply(401, {"detail": {"error": "Unauthorized - Your API key is wrong or missing."}})
                results = [
                    {"title": r["title"], "url": r["url"], "content": r["content"], "score": 0.9, "raw_content": None}
                    for r in fake._results_for(query)
                ]
                return self._reply(200, {"query": query, "follow_up_questions": None, "answer": None, "images": [],
                                         "results": results, "response_time": 0.1})

        return Handler


def read_log(path):
    try:
        with open(path) as f:
            return [json.loads(line) for line in f if line.strip()]
    except FileNotFoundError:
        return []
