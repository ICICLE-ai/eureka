#!/usr/bin/env bash
# Build the Beam pool image, extract its packages, create a token and start the pool.
# Safe to rerun: an existing packages directory and token are kept. After changing
# Beam or Mathlib, delete $PACKAGES first so it is extracted again.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$ROOT"
WORKERS=${WORKERS:-4}

docker build -f lean-beam/deploy/beam-pool/Dockerfile -t beam-pool:local lean-beam

if [ ! -d "$PACKAGES" ]; then
    docker create --name pool-extract beam-pool:local >/dev/null
    trap 'docker rm pool-extract >/dev/null' EXIT
    docker cp pool-extract:/project/.lake/packages "$PACKAGES"
fi

if [ ! -f "$POOL_TOKEN_FILE" ]; then
    (umask 077; python3 -c 'import secrets; print(secrets.token_hex(32))' > "$POOL_TOKEN_FILE")
fi

BEAM_POOL_TOKEN=$(cat "$POOL_TOKEN_FILE") docker compose \
    -f lean-beam/deploy/beam-pool/compose.yaml -f harness/deploy/pool-override.yaml \
    up -d --scale worker="$WORKERS"
