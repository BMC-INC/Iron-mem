#!/usr/bin/env bash
# IronMem: session-end hook
# PreCompact keeps the Claude session alive. Graduate the current observation
# batch, then rotate to a fresh IronMem session for post-compaction activity.

PORT="${IRONMEM_PORT:-37778}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=hooks/lib.sh
source "$SCRIPT_DIR/lib.sh"

HOOK_INPUT="$(cat)"
CLAUDE_SESSION_ID="$(ironmem_hook_field "$HOOK_INPUT" "session_id")"
PROJECT_ROOT="$(ironmem_hook_field "$HOOK_INPUT" "cwd")"
[[ -n "$CLAUDE_SESSION_ID" ]] || exit 0
[[ -n "$PROJECT_ROOT" ]] || PROJECT_ROOT="$(pwd)"
ironmem_rotate_session "$CLAUDE_SESSION_ID" "$PROJECT_ROOT" >/dev/null 2>&1 || true

exit 0
