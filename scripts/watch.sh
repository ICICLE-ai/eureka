#!/usr/bin/env bash
# Open the live dashboard for one or more runs. Ctrl-C only stops the monitor.
#
#   scripts/watch.sh runs/mine [runs/other ...]
set -euo pipefail
source "$(dirname "$0")/env.sh"

: "${1:?usage: scripts/watch.sh runs/mine [runs/other ...]}"
traces=()
for run in "$@"; do
    traces+=("$run/trace.jsonl")
done
exec python3 "$ROOT/harness/trace_report.py" --live "${traces[@]}"
