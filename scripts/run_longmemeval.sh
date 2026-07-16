#!/usr/bin/env bash
# LongMemEval launcher — durable, checkpoint-friendly, PTY-independent.
#
#   scripts/run_longmemeval.sh canary        # no-credit timed dry-run, 5 questions
#   scripts/run_longmemeval.sh full --authorized   # paid 500-question scored run
#
# Implements the handoff run contract (LONGMEMEVAL_HANDOFF_2026-07-15.md):
# release binary, one worker, unique --out with preserved checkpoints, launch
# outside any managed PTY via nohup with a durable log and recorded PID/exit
# status. The paid run refuses to start without --authorized.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-canary}"
AUTH="${2:-}"
DATA="data/longmemeval_s_cleaned.json"
EXPECTED_SHA256="d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BIN="target/release/ironmem"

echo "== preflight =="
[[ -f "$DATA" ]] || { echo "FATAL: dataset missing at $DATA"; exit 2; }
ACTUAL_SHA="$(shasum -a 256 "$DATA" | awk '{print $1}')"
[[ "$ACTUAL_SHA" == "$EXPECTED_SHA256" ]] || { echo "FATAL: dataset hash mismatch"; exit 2; }
echo "dataset ok ($ACTUAL_SHA)"

git diff --quiet -- src/ Cargo.toml Cargo.lock || { echo "FATAL: tracked source is dirty"; exit 2; }
echo "source clean at $(git rev-parse --short HEAD)"

DISK_AVAIL_GB="$(df -g . | awk 'NR==2 {print $4}')"
[[ "$DISK_AVAIL_GB" -ge 10 ]] || { echo "FATAL: only ${DISK_AVAIL_GB}GB free"; exit 2; }

if pmset -g batt | grep -q "Battery Power"; then
  echo "WARNING: on battery — caffeinate -i -s only holds on AC power. Plug in."
fi

if [[ "$MODE" == "full" ]]; then
  [[ "$AUTH" == "--authorized" ]] || {
    echo "FATAL: the full run is a PAID 500-question benchmark."
    echo "Re-run with: scripts/run_longmemeval.sh full --authorized"; exit 2; }
  TOKEN_PREFIX="$( (gcloud auth application-default print-access-token 2>/dev/null || true) | cut -c1-4 )"
  [[ "$TOKEN_PREFIX" == "ya29" ]] || {
    echo "FATAL: Google ADC stale. Run: gcloud auth application-default login"; exit 2; }
  echo "ADC ok"
fi

echo "== build (release, local-onnx) =="
cargo build --release --features local-onnx
echo "binary sha256: $(shasum -a 256 "$BIN" | awk '{print $1}')"

if [[ "$MODE" == "canary" ]]; then
  OUT="docs/evals/longmemeval-canary-timing-$STAMP"
  LOG="$OUT/console.log"
  mkdir -p "$OUT"
  echo "== no-credit timed canary (5 questions, --dry-run) =="
  START_S=$SECONDS
  "$BIN" bench longmemeval --data "$DATA" --out "$OUT" --limit 5 --dry-run \
    2>&1 | tee "$LOG"
  ELAPSED=$((SECONDS - START_S))
  echo "canary wall time: ${ELAPSED}s for 5 questions ($((ELAPSED / 5))s/question)"
  echo "scored-canary reference before batching: 50.8s/question end-to-end"
  exit 0
fi

OUT="docs/evals/longmemeval-full-$STAMP"
LOG="$OUT/console.log"
PIDFILE="$OUT/run.pid"
mkdir -p "$OUT"
echo "== launching full 500-question run =="
echo "out: $OUT (checkpoints preserved here; identical command resumes)"
nohup caffeinate -i -s "$BIN" bench longmemeval --data "$DATA" --out "$OUT" \
  > "$LOG" 2>&1 &
PID=$!
echo "$PID" > "$PIDFILE"
disown "$PID"
echo "pid: $PID (recorded in $PIDFILE)"
echo "monitor:  tail -f $LOG"
echo "resume after any interruption: re-run this exact command with the same --out:"
echo "  $BIN bench longmemeval --data $DATA --out $OUT"
