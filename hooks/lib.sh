#!/usr/bin/env bash
# Shared Claude Code hook helpers. All state is keyed by Claude's own session
# identifier so concurrent windows and subagents never overwrite each other.

ironmem_hook_field() {
  local input="$1"
  local field="$2"
  HOOK_DATA="$input" HOOK_FIELD="$field" python3 -c '
import json, os
try:
    value = json.loads(os.environ.get("HOOK_DATA", "{}")).get(os.environ["HOOK_FIELD"], "")
    print(value if isinstance(value, str) else "")
except Exception:
    print("")
' 2>/dev/null
}

ironmem_state_file() {
  local claude_session_id="$1"
  local state_dir="${IRONMEM_SESSION_DIR:-${HOME}/.ironmem/claude_sessions}"
  [[ "$claude_session_id" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  mkdir -p "$state_dir"
  printf '%s/%s\n' "$state_dir" "$claude_session_id"
}

ironmem_start_session() {
  local claude_session_id="$1"
  local project="$2"
  local port="${IRONMEM_PORT:-37778}"
  local state_file
  state_file="$(ironmem_state_file "$claude_session_id")" || return 1
  local response ironmem_session_id
  response="$(curl -sf -X POST -H "Content-Type: application/json" \
    -d "$(CLAUDE_PROJECT="$project" python3 -c 'import json,os; print(json.dumps({"project":os.environ["CLAUDE_PROJECT"]}))')" \
    "http://127.0.0.1:${port}/session/start" 2>/dev/null)" || return 1
  ironmem_session_id="$(printf '%s' "$response" \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['session_id'])" 2>/dev/null)" || return 1
  [[ -n "$ironmem_session_id" ]] || return 1
  local temporary="${state_file}.tmp.$$"
  printf '%s\n' "$ironmem_session_id" > "$temporary"
  mv "$temporary" "$state_file"
}

ironmem_end_session() {
  local claude_session_id="$1"
  local port="${IRONMEM_PORT:-37778}"
  local state_file
  state_file="$(ironmem_state_file "$claude_session_id")" || return 1
  [[ -f "$state_file" ]] || return 0
  local ironmem_session_id
  ironmem_session_id="$(tr -d '\r\n' < "$state_file")"
  if [[ -n "$ironmem_session_id" ]]; then
    curl -sf -X POST -H "Content-Type: application/json" \
      -d "$(IRONMEM_SESSION_ID="$ironmem_session_id" python3 -c 'import json,os; print(json.dumps({"session_id":os.environ["IRONMEM_SESSION_ID"]}))')" \
      "http://127.0.0.1:${port}/session/end" >/dev/null 2>&1 || return 1
  fi
  rm -f "$state_file"
}

ironmem_rotate_session() {
  local claude_session_id="$1"
  local project="$2"
  ironmem_end_session "$claude_session_id" || return 1
  ironmem_start_session "$claude_session_id" "$project"
}
