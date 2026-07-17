#!/usr/bin/env bash
# Claude SessionEnd is the actual terminal lifecycle event. Close only this
# Claude session's IronMem mapping and do not create a replacement.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=hooks/lib.sh
source "$SCRIPT_DIR/lib.sh"

HOOK_INPUT="$(cat)"
CLAUDE_SESSION_ID="$(ironmem_hook_field "$HOOK_INPUT" "session_id")"
[[ -n "$CLAUDE_SESSION_ID" ]] || exit 0
ironmem_end_session "$CLAUDE_SESSION_ID" >/dev/null 2>&1 || true
exit 0
