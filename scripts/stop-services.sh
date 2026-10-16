#!/usr/bin/env bash
# Shut down the retrieval service, the Tapis bridge and the local Beam pool,
# whichever are running. The pool on Tapis itself keeps running.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$ROOT"

docker compose -f tools/retrieval/compose.yaml down
TAPIS_GATEWAY=unused docker compose -f harness/deploy/tapis-bridge.yaml down
BEAM_POOL_TOKEN=unused docker compose \
    -f lean-beam/deploy/beam-pool/compose.yaml -f harness/deploy/pool-override.yaml \
    down
