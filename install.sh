#!/usr/bin/env bash
# Source install/upgrade, with preserved settings and verified native DB backup.
set -euo pipefail
command -v python3 >/dev/null || { echo 'Python 3.9+ is required.' >&2; exit 1; }
if [[ -n "${BASH_SOURCE[0]:-}" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
else
  SCRIPT_DIR=""
fi
if [[ -z "$SCRIPT_DIR" || ! -f "$SCRIPT_DIR/scripts/install_local.py" ]]; then
  command -v git >/dev/null || { echo 'Git is required for a streamed installer.' >&2; exit 1; }
  IRONMEM_INSTALL_SOURCE="$(mktemp -d "${TMPDIR:-/tmp}/ironmem-install.XXXXXXXX")"
  trap 'rm -rf "$IRONMEM_INSTALL_SOURCE"' EXIT
  git clone --depth 1 https://github.com/BMC-INC/Iron-mem.git "$IRONMEM_INSTALL_SOURCE"
  SCRIPT_DIR="$IRONMEM_INSTALL_SOURCE"
fi
python3 "$SCRIPT_DIR/scripts/install_local.py" --source "$SCRIPT_DIR" "$@"
