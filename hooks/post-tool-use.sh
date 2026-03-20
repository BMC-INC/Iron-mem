#!/usr/bin/env bash
# IronMem: post-tool-use hook
# Records each tool call observation to the ironmem worker.
# Fails silently so it never interrupts Claude Code.

PORT="${IRONMEM_PORT:-37778}"
SESSION_FILE="${HOME}/.ironmem/current_session"

# Bail immediately if no session file
if [ ! -f "$SESSION_FILE" ]; then
  exit 0
fi

SESSION_ID="$(cat "$SESSION_FILE" 2>/dev/null || echo "")"
if [ -z "$SESSION_ID" ]; then
  exit 0
fi

# Resolve project
PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# Claude Code exposes these env vars in PostToolUse hooks
TOOL_NAME="${CLAUDE_TOOL_NAME:-unknown}"
TOOL_INPUT="${CLAUDE_TOOL_INPUT:-}"
TOOL_OUTPUT="${CLAUDE_TOOL_OUTPUT:-}"

# Truncate large values before sending (shell-level safety net)
TOOL_INPUT="${TOOL_INPUT:0:2000}"
TOOL_OUTPUT="${TOOL_OUTPUT:0:2000}"

# Build JSON payload
PAYLOAD=$(cat <<EOF
{
  "session_id": "$SESSION_ID",
  "project": "$PROJECT_ROOT",
  "tool": "$TOOL_NAME",
  "input": $(echo "$TOOL_INPUT" | python3 -c "import json,sys; print(json.dumps(sys.stdin.read()))" 2>/dev/null || echo "null"),
  "output": $(echo "$TOOL_OUTPUT" | python3 -c "import json,sys; print(json.dumps(sys.stdin.read()))" 2>/dev/null || echo "null")
}
EOF
)

curl -sf \
  -X POST \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  "http://127.0.0.1:${PORT}/event" > /dev/null 2>&1 || true

exit 0
