#!/usr/bin/env bash
# IronMem: session-end hook
# Belt-and-suspenders: same as stop.sh but triggered by SessionEnd lifecycle event.
# Also handles starting a new session ID for the next session.

PORT="${IRONMEM_PORT:-37778}"
SESSION_FILE="${HOME}/.ironmem/current_session"
IRONMEM_BIN="${HOME}/.ironmem/bin/ironmem"

# End any existing session
if [ -f "$SESSION_FILE" ]; then
  SESSION_ID="$(cat "$SESSION_FILE" 2>/dev/null || echo "")"
  if [ -n "$SESSION_ID" ]; then
    curl -sf \
      -X POST \
      -H "Content-Type: application/json" \
      -d "{\"session_id\": \"$SESSION_ID\"}" \
      "http://127.0.0.1:${PORT}/session/end" > /dev/null 2>&1 || true
  fi
  rm -f "$SESSION_FILE"
fi

# Register new session for the upcoming next start
PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
NEW_SESSION=$(curl -sf \
  -X POST \
  -H "Content-Type: application/json" \
  -d "{\"project\": \"$PROJECT_ROOT\"}" \
  "http://127.0.0.1:${PORT}/session/start" 2>/dev/null || echo "")

if [ -n "$NEW_SESSION" ]; then
  # Extract session_id from JSON response
  SESSION_ID=$(echo "$NEW_SESSION" | python3 -c "import json,sys; print(json.load(sys.stdin)['session_id'])" 2>/dev/null || echo "")
  if [ -n "$SESSION_ID" ]; then
    mkdir -p "${HOME}/.ironmem"
    echo "$SESSION_ID" > "$SESSION_FILE"
  fi
fi

exit 0
