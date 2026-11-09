#!/usr/bin/env bash
# Runs every Rust runtime built for this host over one corpus, one after another,
# with peak memory from /usr/bin/time.
#
#   scripts/benchmarks/rust_asr/run.sh <corpus dir> <out dir>
#
# The harness is built first (see docs/experiments/2026-11-07-rust-asr-runtimes.md):
#   macOS: cargo build --release --features wavo,gigastt,coreml
#   Linux: cargo build --release --features wavo,gigastt,cuda
# Models: HF_HOME for wavo (pulled on first use), GIGASTT_MODEL_DIR for gigastt
# (`gigastt download --model-variant e2e_rnnt --model-dir ...`).
set -euo pipefail
B=$(cd "$(dirname "$0")" && pwd)
CORPUS=$(cd "$1" && pwd); mkdir -p "$2"; OUT=$(cd "$2" && pwd)
BIN=${ASRBENCH:-$B/harness/target/release/asrbench}
PY=${RECOG_PYTHON:-python3}

if [ "$(uname -s)" = Darwin ]; then
    TIME=(/usr/bin/time -l); RUNS=("wavo gpu" "wavo ane" "gigastt coreml" "gigastt cpu")
else
    TIME=(/usr/bin/time -v); RUNS=("wavo gpu" "gigastt cuda" "gigastt cpu")
fi

for run in "${RUNS[@]}"; do
    read -r runtime backend <<< "$run"
    "${TIME[@]}" "$BIN" "$runtime" "$CORPUS" "$OUT/$runtime-$backend.json" "$backend" 2> "$OUT/$runtime-$backend.log"
done
"$PY" "$B/score.py" "$CORPUS" "$OUT"
