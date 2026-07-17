#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$TMP/home/.ironmem/bin"
cat > "$TMP/home/.ironmem/bin/ironmem" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$TMP/home/.ironmem/bin/ironmem"

PORT_FILE="$TMP/port"
EVENT_LOG="$TMP/events.jsonl"
python3 - "$PORT_FILE" "$EVENT_LOG" <<'PY' &
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file, event_log = sys.argv[1:3]
counter = 0

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'{"ok":true}')

    def do_POST(self):
        global counter
        size = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(size) or b"{}")
        with open(event_log, "a") as stream:
            stream.write(json.dumps({"path": self.path, "body": body}) + "\n")
        if self.path == "/session/start":
            counter += 1
            response = {"session_id": f"iron-{counter}"}
        else:
            response = {"ok": True}
        payload = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as stream:
    stream.write(str(server.server_address[1]))
server.serve_forever()
PY
SERVER_PID=$!

for _ in {1..100}; do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.02
done
[[ -s "$PORT_FILE" ]] || { echo "mock server did not start"; exit 1; }
PORT="$(cat "$PORT_FILE")"

export HOME="$TMP/home"
export IRONMEM_PORT="$PORT"
export IRONMEM_BIN="$TMP/home/.ironmem/bin/ironmem"

payload_a='{"session_id":"claude-a","cwd":"/tmp/project-a"}'
payload_b='{"session_id":"claude-b","cwd":"/tmp/project-b"}'
printf '%s' "$payload_a" | "$ROOT/hooks/session-start.sh"
printf '%s' "$payload_b" | "$ROOT/hooks/session-start.sh"

state_a="$HOME/.ironmem/claude_sessions/claude-a"
state_b="$HOME/.ironmem/claude_sessions/claude-b"
[[ "$(cat "$state_a")" == "iron-1" ]]
[[ "$(cat "$state_b")" == "iron-2" ]]

printf '%s' '{"session_id":"claude-a","cwd":"/tmp/project-a","tool_name":"Read","tool_input":{"file":"a"},"tool_response":{"ok":true}}' \
  | "$ROOT/hooks/post-tool-use.sh"
printf '%s' '{"session_id":"claude-b","cwd":"/tmp/project-b","tool_name":"Read","tool_input":{"file":"b"},"tool_response":{"ok":true}}' \
  | "$ROOT/hooks/post-tool-use.sh"

printf '%s' "$payload_a" | "$ROOT/hooks/stop.sh"
[[ "$(cat "$state_a")" == "iron-3" ]]
[[ "$(cat "$state_b")" == "iron-2" ]]

printf '%s' "$payload_b" | "$ROOT/hooks/session-close.sh"
[[ ! -e "$state_b" ]]
[[ -e "$state_a" ]]

python3 - "$EVENT_LOG" <<'PY'
import json
import sys

events = [json.loads(line) for line in open(sys.argv[1])]
recorded = [entry["body"]["session_id"] for entry in events if entry["path"] == "/event"]
assert recorded == ["iron-1", "iron-2"], recorded
ended = [entry["body"]["session_id"] for entry in events if entry["path"] == "/session/end"]
assert "iron-1" in ended and "iron-2" in ended, ended
PY

echo "Claude hook isolation/lifecycle test passed"
