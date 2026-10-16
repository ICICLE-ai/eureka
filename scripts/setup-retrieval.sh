#!/usr/bin/env bash
# Optional: build and start the retrieval tools (Loogle and lemma search).
# Needs the pool from setup-pool.sh. Then pass RETRIEVAL=1 to run.sh.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$ROOT"

docker build --target client -t eureka-agent:local tools/retrieval
docker compose -f tools/retrieval/compose.yaml up -d --build --wait
