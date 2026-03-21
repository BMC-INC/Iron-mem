#!/usr/bin/env bash
# IronMem: session-start hook
# Injects recent session memories into IRONMEM.md and ensures CLAUDE.md imports it.
# Fails silently so it never interrupts Claude Code.

set -euo pipefail

IRONMEM_BIN="${HOME}/.ironmem/bin/ironmem"
PORT="${IRONMEM_PORT:-37778}"
LIMIT="${IRONMEM_INJECT_LIMIT:-5}"

# Resolve git root of the current project
PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# Fail silently if binary not found
if [ ! -x "$IRONMEM_BIN" ]; then
  exit 0
fi

# Check if server is running; start it if not
if ! curl -sf "http://127.0.0.1:${PORT}/status" > /dev/null 2>&1; then
  # Start server in background, detached from this shell
  nohup "$IRONMEM_BIN" server > "${HOME}/.ironmem/server.log" 2>&1 &
  # Give it a moment to start
  sleep 0.5
fi

# Inject memories into IRONMEM.md and update CLAUDE.md
"$IRONMEM_BIN" inject --project "$PROJECT_ROOT" --limit "$LIMIT" > /dev/null 2>&1 || true

# Start a new session and write the session ID so post-tool-use can record observations
SESSION_ID=$(curl -sf \
  -X POST \
  -H "Content-Type: application/json" \
  -d "{\"project\": \"$PROJECT_ROOT\"}" \
  "http://127.0.0.1:${PORT}/session/start" | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['session_id'])" 2>/dev/null || echo "")

if [ -n "$SESSION_ID" ]; then
  echo "$SESSION_ID" > "${HOME}/.ironmem/current_session"
fi

exit 0
