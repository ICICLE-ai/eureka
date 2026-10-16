#!/usr/bin/env bash
# Connect the sandboxes to the Beam pool on Tapis and check that it answers.
#
#   scripts/connect-tapis.sh
#
# Starts the bridge, prints the pool's status, builds a sandbox image that matches
# the Tapis snapshot if there isn't one yet (see build-tapis-image.sh), and
# extracts that image's packages. Safe to rerun.
#
# Settings (environment):
#   TAPIS_GATEWAY          gateway host (default: infinileanpool.pods.tacc.tapis.io).
#                          `lean-beam/deploy/beam-pool/tapis.py status` prints it.
#   TAPIS_POOL_TOKEN_FILE  the pool's BEAM_POOL_TOKEN (default: .secrets/tapis-pool.token)
#   IMAGE                  sandbox image (default: eureka-tapis:local)
#   PACKAGES               where its packages go (default: pool-packages-tapis/)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
: "${PACKAGES:=$ROOT/pool-packages-tapis}"
source "$ROOT/scripts/env.sh"
cd "$ROOT"
export TAPIS_GATEWAY=${TAPIS_GATEWAY:-infinileanpool.pods.tacc.tapis.io}
export TAPIS_POOL_TOKEN_FILE=${TAPIS_POOL_TOKEN_FILE:-$ROOT/.secrets/tapis-pool.token}
IMAGE=${IMAGE:-eureka-tapis:local}

# A fresh checkout may never have started a local pool. The bridge and snapshot
# checks still need its base image, but no local workers need to be started.
if ! docker image inspect beam-pool:local >/dev/null 2>&1; then
    docker build -f lean-beam/deploy/beam-pool/Dockerfile -t beam-pool:local lean-beam
fi

# The local pool creates this network too; create it here if that pool isn't used.
if ! docker network inspect harness-agents >/dev/null 2>&1; then
    docker network create --internal harness-agents >/dev/null
fi
docker compose -f harness/deploy/tapis-bridge.yaml up -d

# Passed by name so the token doesn't appear in the process list.
BEAM_POOL_TOKEN=$(cat "$TAPIS_POOL_TOKEN_FILE")
export BEAM_POOL_TOKEN
docker run --rm --network harness-agents -w /opt/beam -e PYTHONPATH=/opt/beam -e BEAM_POOL_TOKEN \
    --entrypoint python3 beam-pool:local -m scripts.beam_pool status --endpoint tapis-gateway:9000
echo

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    OUTPUT_IMAGE=$IMAGE "$ROOT/scripts/build-tapis-image.sh"
fi
if [ ! -d "$PACKAGES" ]; then
    docker create --name pool-extract "$IMAGE" >/dev/null
    trap 'docker rm pool-extract >/dev/null' EXIT
    docker cp pool-extract:/project/.lake/packages "$PACKAGES"
fi
echo "Connected. Start a run with: scripts/run-tapis.sh runs/problem runs/mine"
