#!/usr/bin/env bash
# IronMem: post-tool-use hook
# Records each tool call observation to the ironmem worker.
# Claude Code passes hook data via stdin as JSON.
# Fails silently so it never interrupts Claude Code.

PORT="${IRONMEM_PORT:-37778}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=hooks/lib.sh
source "$SCRIPT_DIR/lib.sh"

# Read hook data from stdin (Claude Code sends JSON via stdin)
HOOK_INPUT="$(cat)"
if [ -z "$HOOK_INPUT" ]; then
  exit 0
fi
CLAUDE_SESSION_ID="$(ironmem_hook_field "$HOOK_INPUT" "session_id")"
[[ -n "$CLAUDE_SESSION_ID" ]] || exit 0
SESSION_FILE="$(ironmem_state_file "$CLAUDE_SESSION_ID")" || exit 0
[[ -f "$SESSION_FILE" ]] || exit 0
SESSION_ID="$(tr -d '\r\n' < "$SESSION_FILE")"
[[ -n "$SESSION_ID" ]] || exit 0

# Build the event payload in Python, passing hook data via env to avoid quoting issues
PAYLOAD="$(HOOK_DATA="$HOOK_INPUT" SESS_ID="$SESSION_ID" python3 -c "
import json, os

hook = json.loads(os.environ['HOOK_DATA'])
session_id = os.environ['SESS_ID']

project = hook.get('cwd', '')
tool_name = hook.get('tool_name', 'unknown')
tool_input = json.dumps(hook.get('tool_input', {}))[:2000]
tool_output = json.dumps(hook.get('tool_response', {}))[:2000]

print(json.dumps({
    'session_id': session_id,
    'project': project,
    'tool': tool_name,
    'input': tool_input,
    'output': tool_output,
}))
" 2>/dev/null)"

if [ -z "$PAYLOAD" ]; then
  exit 0
fi

curl -sf \
  -X POST \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  "http://127.0.0.1:${PORT}/event" > /dev/null 2>&1 || true

exit 0
