#!/usr/bin/env bash
# IronMem: session-start hook
# Injects recent session memories into IRONMEM.md and ensures CLAUDE.md imports it.
# Fails silently so it never interrupts Claude Code.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=hooks/lib.sh
source "$SCRIPT_DIR/lib.sh"

HOOK_INPUT="$(cat)"
CLAUDE_SESSION_ID="$(ironmem_hook_field "$HOOK_INPUT" "session_id")"
HOOK_CWD="$(ironmem_hook_field "$HOOK_INPUT" "cwd")"
[[ -n "$CLAUDE_SESSION_ID" ]] || exit 0

IRONMEM_BIN="${IRONMEM_BIN:-${HOME}/.ironmem/bin/ironmem}"
PORT="${IRONMEM_PORT:-37778}"
LIMIT="${IRONMEM_INJECT_LIMIT:-5}"

# Resolve git root of the current project
if [[ -n "$HOOK_CWD" && -d "$HOOK_CWD" ]]; then
  PROJECT_ROOT="$(git -C "$HOOK_CWD" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$HOOK_CWD")"
else
  PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

# Fail silently if binary not found
if [ ! -x "$IRONMEM_BIN" ]; then
  exit 0
fi

# Persist API key to file so the server can read it even when launched via nohup
# (nohup/sandbox may strip environment variables from child processes)
if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  echo "$ANTHROPIC_API_KEY" > "${HOME}/.ironmem/api_key"
  chmod 600 "${HOME}/.ironmem/api_key"
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

# Duplicate lifecycle starts close only this Claude session's prior mapping.
ironmem_end_session "$CLAUDE_SESSION_ID" >/dev/null 2>&1 || true
ironmem_start_session "$CLAUDE_SESSION_ID" "$PROJECT_ROOT" >/dev/null 2>&1 || true

exit 0
