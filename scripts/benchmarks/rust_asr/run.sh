#!/usr/bin/env bash
# Runs recog's PyTorch model (ref.py) and every Rust runtime built for this host over
# one corpus, one after another, with peak memory from /usr/bin/time.
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
REPO=$(cd "$B/../../.." && pwd)
CORPUS=$(cd "$1" && pwd); mkdir -p "$2"; OUT=$(cd "$2" && pwd)
BIN=${ASRBENCH:-$B/harness/target/release/asrbench}
PY=${RECOG_PYTHON:-$REPO/venv-gigaam/bin/python}

if [ "$(uname -s)" = Darwin ]; then
    TIME=(/usr/bin/time -l); DEVICE=mps; RUNS=("wavo gpu" "wavo ane" "gigastt coreml" "gigastt cpu")
else
    TIME=(/usr/bin/time -v); DEVICE=${RECOG_ASR_DEVICE:-cuda}; RUNS=("wavo gpu" "gigastt cuda" "gigastt cpu")
fi

(cd "$REPO" && PYTHONPATH="$REPO" "${TIME[@]}" "$PY" "$B/ref.py" "$CORPUS" "$OUT/pytorch-$DEVICE.json" "$DEVICE") \
    2> "$OUT/pytorch-$DEVICE.log"
for run in "${RUNS[@]}"; do
    read -r runtime backend <<< "$run"
    "${TIME[@]}" "$BIN" "$runtime" "$CORPUS" "$OUT/$runtime-$backend.json" "$backend" 2> "$OUT/$runtime-$backend.log"
done
"$PY" "$B/score.py" "$CORPUS" "$OUT"
