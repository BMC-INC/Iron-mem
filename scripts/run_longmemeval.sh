#!/usr/bin/env bash
# LongMemEval launcher — durable, checkpoint-friendly, PTY-independent.
#
#   scripts/run_longmemeval.sh canary
#   scripts/run_longmemeval.sh scored-canary --authorized
#   scripts/run_longmemeval.sh full --authorized
#
# Implements the handoff run contract (LONGMEMEVAL_HANDOFF_2026-07-15.md):
# release binary, one worker, unique --out with preserved checkpoints, launch
# outside any managed PTY via launchd with a durable log and recorded PID/exit
# status. The paid run refuses to start without --authorized.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT_PATH="$SCRIPT_DIR/$(basename "$0")"
cd "$SCRIPT_DIR/.."
BIN="$PWD/target/release/ironmem"

# Internal detached-worker entry point. The parent launcher exits immediately,
# while this wrapper waits for the benchmark and atomically records its exit
# code and completion time. A missing status file therefore means the wrapper
# itself was forcibly terminated before it could report an outcome.
if [[ "${1:-}" == "__record-exit" ]]; then
  [[ "$#" -ge 3 ]] || { echo "FATAL: __record-exit needs STATUS_FILE COMMAND..."; exit 2; }
  STATUS_FILE="$2"
  shift 2
  GATE_FILE=""
  if [[ "${1:-}" == "--gate" ]]; then
    [[ "$#" -ge 3 ]] || { echo "FATAL: --gate needs GATE_FILE COMMAND..."; exit 2; }
    GATE_FILE="$2"
    shift 2
  fi
  set +e
  "$@"
  COMMAND_EXIT=$?
  if [[ "$COMMAND_EXIT" -eq 0 && -n "$GATE_FILE" ]]; then
    GATE_TMP="${GATE_FILE}.tmp.$$"
    {
      echo "binary_sha256=$(shasum -a 256 "$BIN" | awk '{print $1}')"
      echo "passed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$GATE_TMP"
    mv "$GATE_TMP" "$GATE_FILE"
  fi
  STATUS_TMP="${STATUS_FILE}.tmp.$$"
  {
    echo "exit_code=$COMMAND_EXIT"
    echo "finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$STATUS_TMP"
  mv "$STATUS_TMP" "$STATUS_FILE"
  exit "$COMMAND_EXIT"
fi

launch_once() {
  local label="$1"
  local out_abs="$2"
  local status_file="$3"
  local gate_file="$4"
  shift 4
  local log="$out_abs/console.log"
  local plist="$out_abs/launchd.plist"
  local label_file="$out_abs/launchd.label"
  local pid_file="$out_abs/run.pid"
  local -a wrapper=("$SCRIPT_PATH" "__record-exit" "$status_file")
  if [[ -n "$gate_file" ]]; then
    wrapper+=("--gate" "$gate_file")
  fi
  wrapper+=("$@")

  {
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    echo '<plist version="1.0"><dict>'
    echo '<key>Label</key>'
    printf '<string>%s</string>\n' "$label"
    echo '<key>ProgramArguments</key><array>'
    local arg escaped
    for arg in "${wrapper[@]}"; do
      escaped="${arg//&/&amp;}"
      escaped="${escaped//</&lt;}"
      escaped="${escaped//>/&gt;}"
      printf '<string>%s</string>\n' "$escaped"
    done
    echo '</array>'
    echo '<key>RunAtLoad</key><true/>'
    echo '<key>KeepAlive</key><false/>'
    echo '<key>ProcessType</key><string>Background</string>'
    printf '<key>StandardOutPath</key><string>%s</string>\n' "$log"
    printf '<key>StandardErrorPath</key><string>%s</string>\n' "$log"
    echo '</dict></plist>'
  } > "$plist"
  plutil -lint "$plist" >/dev/null
  launchctl bootstrap "gui/$(id -u)" "$plist"
  echo "$label" > "$label_file"

  local pid=""
  local attempt
  for attempt in {1..20}; do
    pid="$(launchctl print "gui/$(id -u)/$label" 2>/dev/null \
      | awk '/pid =/ {print $3; exit}')"
    [[ -n "$pid" ]] && break
    sleep 0.1
  done
  [[ -n "$pid" ]] || {
    echo "FATAL: launchd accepted the one-shot job but exposed no PID"
    exit 2
  }
  echo "$pid" > "$pid_file"
  echo "pid: $pid (recorded in $pid_file)"
  echo "launchd label: $label (one-shot; KeepAlive=false)"
  echo "monitor: tail -f $log"
  echo "completion: cat $status_file"
}

MODE="${1:-canary}"
AUTH="${2:-}"
DATA="data/longmemeval_s_cleaned.json"
EXPECTED_SHA256="d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"

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

if [[ "$MODE" == "full" || "$MODE" == "scored-canary" ]]; then
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
  echo "== no-credit stratified canary (one question per ability, --dry-run) =="
  START_S=$SECONDS
  "$BIN" bench longmemeval --data "$DATA" --out "$OUT" \
    --stratified-per-ability 1 --retrieve-k 25 --dry-run \
    2>&1 | tee "$LOG"
  ELAPSED=$((SECONDS - START_S))
  echo "canary wall time: ${ELAPSED}s for 6 representative questions"
  exit 0
fi

if [[ "$MODE" == "scored-canary" ]]; then
  OUT="docs/evals/longmemeval-scored-canary-$STAMP"
else
  [[ "$MODE" == "full" ]] || {
    echo "FATAL: mode must be canary, scored-canary, or full"
    exit 2
  }
  OUT="docs/evals/longmemeval-full-$STAMP"
fi
mkdir -p "$OUT"
OUT_ABS="$PWD/$OUT"
LOG="$OUT_ABS/console.log"
PIDFILE="$OUT_ABS/run.pid"
STATUSFILE="$OUT_ABS/exit.status"
LABELFILE="$OUT_ABS/launchd.label"
GATEFILE=""
LAUNCH_LABEL="com.execlayer.ironmem.longmemeval.$STAMP"

if [[ "$MODE" == "scored-canary" ]]; then
  GATEFILE="$OUT_ABS/scored-canary.gate"
  echo "== launching paid stratified canary (12 questions; two per ability) =="
  BENCH_ARGS=(
    /usr/bin/env "PATH=$PATH" "HOME=$HOME"
    "CLOUDSDK_CONFIG=${CLOUDSDK_CONFIG:-$HOME/.config/gcloud}"
    /usr/bin/caffeinate -i -s "$PWD/$BIN" bench longmemeval
    --data "$PWD/$DATA" --out "$OUT_ABS"
    --stratified-per-ability 2 --retrieve-k 25
    --answer-model gemini-2.5-flash --judge-model gemini-2.5-pro
    --min-accuracy 0.50
  )
else
  CURRENT_BIN_SHA="$(shasum -a 256 "$BIN" | awk '{print $1}')"
  PASSED_GATE="$(find docs/evals -maxdepth 2 -name scored-canary.gate -type f \
    -exec grep -l "binary_sha256=$CURRENT_BIN_SHA" {} + 2>/dev/null \
    | sort | tail -n 1)"
  [[ -n "$PASSED_GATE" ]] || {
    echo "FATAL: this exact binary has not passed the paid stratified canary."
    echo "Run: scripts/run_longmemeval.sh scored-canary --authorized"
    exit 2
  }
  echo "canary gate ok ($PASSED_GATE)"
  echo "== launching full 500-question run =="
  BENCH_ARGS=(
    /usr/bin/env "PATH=$PATH" "HOME=$HOME"
    "CLOUDSDK_CONFIG=${CLOUDSDK_CONFIG:-$HOME/.config/gcloud}"
    /usr/bin/caffeinate -i -s "$PWD/$BIN" bench longmemeval
    --data "$PWD/$DATA" --out "$OUT_ABS"
    --retrieve-k 25
    --answer-model gemini-2.5-flash --judge-model gemini-2.5-pro
  )
fi

echo "out: $OUT (checkpoints preserved here; identical command resumes)"
launch_once "$LAUNCH_LABEL" "$OUT_ABS" "$STATUSFILE" "$GATEFILE" "${BENCH_ARGS[@]}"
