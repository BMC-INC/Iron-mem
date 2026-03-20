#!/usr/bin/env bash
# IronMem: stop hook
# Ends the current session and triggers AI compression.
# Called when Claude Code session stops.
# Fails silently.

PORT="${IRONMEM_PORT:-37778}"
SESSION_FILE="${HOME}/.ironmem/current_session"

if [ ! -f "$SESSION_FILE" ]; then
  exit 0
fi

SESSION_ID="$(cat "$SESSION_FILE" 2>/dev/null || echo "")"
if [ -z "$SESSION_ID" ]; then
  exit 0
fi

# End session (triggers compression server-side)
curl -sf \
  -X POST \
  -H "Content-Type: application/json" \
  -d "{\"session_id\": \"$SESSION_ID\"}" \
  "http://127.0.0.1:${PORT}/session/end" > /dev/null 2>&1 || true

# Clear session file
rm -f "$SESSION_FILE"

exit 0
