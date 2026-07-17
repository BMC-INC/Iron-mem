#!/usr/bin/env bash
# IronMem: stop hook
# Claude Stop fires after each response, not only when the interactive session
# closes. Graduate the current batch and immediately rotate to a fresh IronMem
# session so subsequent turns keep recording.
# Fails silently.

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
