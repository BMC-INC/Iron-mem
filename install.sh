#!/usr/bin/env bash
# IronMem installer
# Builds the release binary, installs hooks into ~/.claude/hooks/,
# and sets up ~/.ironmem/ directory structure.

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

HOOKS=(
  "session-start.sh:PreToolUse"
  "post-tool-use.sh:PostToolUse"
  "stop.sh:Stop"
  "session-end.sh:PostToolUse"
)

for entry in "${HOOKS[@]}"; do
  src="${entry%%:*}"
  dest_name="${entry##*:}"
  src_path="${SCRIPT_DIR}/hooks/${src}"
  dest_path="${CLAUDE_HOOKS_DIR}/${src}"

  cp "$src_path" "$dest_path"
  chmod +x "$dest_path"
  info "  Hook installed: $dest_path"
done

success "Hooks installed to $CLAUDE_HOOKS_DIR"

# ── 5. Create ~/.ironmem directory structure ───────────────────────────────────
mkdir -p "${IRONMEM_HOME}"
touch "${IRONMEM_HOME}/server.log"

success "Directory structure created at $IRONMEM_HOME"

# ── 6. Initialize config (if not present) ─────────────────────────────────────
if [ ! -f "${IRONMEM_HOME}/settings.json" ]; then
  "$IRONMEM_BIN" config > /dev/null 2>&1 || true
  info "Default config created at ${IRONMEM_HOME}/settings.json"
fi

# ── 7. PATH check ──────────────────────────────────────────────────────────────
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

# ── 8. API key check ───────────────────────────────────────────────────────────
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
