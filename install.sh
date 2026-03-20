#!/usr/bin/env bash
# IronMem installer
# Builds the release binary, installs hooks into ~/.claude/hooks/,
# registers hooks in Claude Code settings.json, and sets up ~/.ironmem/

set -euo pipefail

BOLD='\033[1m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()    { echo -e "${BOLD}[ironmem]${NC} $1"; }
success() { echo -e "${GREEN}✅ $1${NC}"; }
warn()    { echo -e "${YELLOW}⚠️  $1${NC}"; }
error()   { echo -e "${RED}❌ $1${NC}"; exit 1; }

IRONMEM_HOME="${HOME}/.ironmem"
IRONMEM_BIN="${IRONMEM_HOME}/bin/ironmem"
CLAUDE_HOOKS_DIR="${HOME}/.claude/hooks"
CLAUDE_SETTINGS="${HOME}/.claude/settings.json"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

info "Installing IronMem — persistent session memory for AI coding assistants"
echo ""

# ── 1. Check Rust ──────────────────────────────────────────────────────────────
if ! command -v cargo &> /dev/null; then
  error "Rust/Cargo not found. Install from https://rustup.rs"
fi

RUST_VERSION=$(rustc --version)
info "Found $RUST_VERSION"

# ── 2. Build release binary ────────────────────────────────────────────────────
info "Building release binary (this takes ~1-2 min first time)..."
cd "$SCRIPT_DIR"
cargo build --release 2>&1 | tail -5
success "Build complete"

# ── 3. Install binary ──────────────────────────────────────────────────────────
mkdir -p "${IRONMEM_HOME}/bin"
cp "target/release/ironmem" "$IRONMEM_BIN"
chmod +x "$IRONMEM_BIN"
success "Binary installed to $IRONMEM_BIN"

# ── 4. Install Claude Code hooks ───────────────────────────────────────────────
mkdir -p "$CLAUDE_HOOKS_DIR"

for src in session-start.sh post-tool-use.sh stop.sh session-end.sh; do
  cp "${SCRIPT_DIR}/hooks/${src}" "${CLAUDE_HOOKS_DIR}/${src}"
  chmod +x "${CLAUDE_HOOKS_DIR}/${src}"
  info "  Hook installed: ${CLAUDE_HOOKS_DIR}/${src}"
done

success "Hooks installed to $CLAUDE_HOOKS_DIR"

# ── 5. Register hooks in Claude Code settings.json ────────────────────────────
mkdir -p "${HOME}/.claude"

if [ ! -f "$CLAUDE_SETTINGS" ]; then
  cat > "$CLAUDE_SETTINGS" << EOF
{
  "hooks": {
    "SessionStart": [{"type": "command", "command": "${CLAUDE_HOOKS_DIR}/session-start.sh"}],
    "PostToolUse":  [{"type": "command", "command": "${CLAUDE_HOOKS_DIR}/post-tool-use.sh"}],
    "Stop":         [{"type": "command", "command": "${CLAUDE_HOOKS_DIR}/stop.sh"}],
    "PreCompact":   [{"type": "command", "command": "${CLAUDE_HOOKS_DIR}/session-end.sh"}]
  }
}
EOF
  success "Created Claude Code settings.json with hooks registered"
else
  # Merge hooks into existing settings.json using Python
  python3 - "$CLAUDE_SETTINGS" "$CLAUDE_HOOKS_DIR" << 'PYEOF'
import json, sys

settings_path = sys.argv[1]
hooks_dir = sys.argv[2]

with open(settings_path, 'r') as f:
    settings = json.load(f)

if 'hooks' in settings:
    print("[ironmem] 'hooks' key already exists in settings.json — skipping.")
    print("[ironmem] If IronMem hooks are missing, manually add them to:", settings_path)
else:
    settings['hooks'] = {
        "SessionStart": [{"type": "command", "command": f"{hooks_dir}/session-start.sh"}],
        "PostToolUse":  [{"type": "command", "command": f"{hooks_dir}/post-tool-use.sh"}],
        "Stop":         [{"type": "command", "command": f"{hooks_dir}/stop.sh"}],
        "PreCompact":   [{"type": "command", "command": f"{hooks_dir}/session-end.sh"}]
    }
    with open(settings_path, 'w') as f:
        json.dump(settings, f, indent=2)
    print(f"✅ Hooks registered in {settings_path}")
PYEOF
fi

# ── 6. Create ~/.ironmem directory structure ───────────────────────────────────
mkdir -p "${IRONMEM_HOME}"
touch "${IRONMEM_HOME}/server.log"

success "Directory structure created at $IRONMEM_HOME"

# ── 7. Initialize config (if not present) ─────────────────────────────────────
if [ ! -f "${IRONMEM_HOME}/settings.json" ]; then
  "$IRONMEM_BIN" config > /dev/null 2>&1 || true
  info "Default config created at ${IRONMEM_HOME}/settings.json"
fi

# ── 8. PATH check ──────────────────────────────────────────────────────────────
echo ""
info "Checking PATH..."
if echo "$PATH" | grep -q "${IRONMEM_HOME}/bin"; then
  success "~/.ironmem/bin is already in your PATH"
else
  warn "Add this to your shell profile (~/.zshrc or ~/.bashrc):"
  echo ""
  echo "  export PATH=\"\$HOME/.ironmem/bin:\$PATH\""
  echo ""
fi

# ── 9. API key check ───────────────────────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
  warn "ANTHROPIC_API_KEY is not set."
  warn "AI compression will not work until you add it to your shell profile:"
  echo ""
  echo "  export ANTHROPIC_API_KEY=\"your-key-here\""
  echo ""
fi

# ── Done ───────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
success "IronMem installed successfully!"
echo ""
echo "Commands:"
echo "  ironmem server         Start the worker"
echo "  ironmem status         Check worker + DB stats"
echo "  ironmem list           List recent memories for current project"
echo "  ironmem search <query> Full-text search memories"
echo "  ironmem wipe           Delete all memories for current project"
echo "  ironmem inject         Manually inject context into IRONMEM.md"
echo ""
echo "Restart Claude Code to activate hooks."
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
