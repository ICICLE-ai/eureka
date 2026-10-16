#!/usr/bin/env bash
# Build a sandbox image whose project matches the snapshot on the Tapis workers.
#
# The Tapis workers run an amd64 image. An arm64 build of the same project has
# identical sources and .olean files, but a few Lake .trace files record the build
# platform, and attaching requires every file to match. This copies the differing
# files out of the amd64 image (without running it) onto beam-pool:local.
# connect-tapis.sh runs this when the image is missing; run it yourself after the
# Tapis image changes. On an amd64 machine you can skip it and set IMAGE to the
# Tapis image directly.
#
# Settings (environment):
#   TAPIS_SOURCE_IMAGE     the image the Tapis workers run (default: debganguly/infinilean:0.0.1)
#   TAPIS_POOL_TOKEN_FILE  as in connect-tapis.sh
#   BASE_IMAGE             default beam-pool:local; OUTPUT_IMAGE default eureka-tapis:local
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$ROOT"
TAPIS_SOURCE_IMAGE=${TAPIS_SOURCE_IMAGE:-debganguly/infinilean:0.0.1}
TAPIS_POOL_TOKEN_FILE=${TAPIS_POOL_TOKEN_FILE:-$ROOT/.secrets/tapis-pool.token}
BASE_IMAGE=${BASE_IMAGE:-beam-pool:local}
OUTPUT_IMAGE=${OUTPUT_IMAGE:-eureka-tapis:local}

BEAM_POOL_TOKEN=$(cat "$TAPIS_POOL_TOKEN_FILE")
export BEAM_POOL_TOKEN
diff_against() {
    docker run --rm --network harness-agents -w /opt/beam -e PYTHONPATH=/opt/beam -e BEAM_POOL_TOKEN \
        -v "$ROOT/scripts/tapis-snapshot-diff.py:/tmp/diff.py:ro" --entrypoint python3 "$1" /tmp/diff.py
}

context=$(mktemp -d)
trap 'rm -rf "$context"; docker rm tapis-source >/dev/null 2>&1 || true' EXIT
differing=$(diff_against "$BASE_IMAGE")
mapfile -t files <<< "$differing"
if [ -z "$differing" ]; then
    docker tag "$BASE_IMAGE" "$OUTPUT_IMAGE"
    echo "$BASE_IMAGE already matches the Tapis snapshot; tagged it $OUTPUT_IMAGE."
    exit 0
fi
echo "${#files[@]} files differ from the Tapis snapshot"

docker create --platform linux/amd64 --name tapis-source "$TAPIS_SOURCE_IMAGE" >/dev/null
for file in "${files[@]}"; do
    mkdir -p "$context/project/$(dirname "$file")"
    docker cp "tapis-source:/project/$file" "$context/project/$file"
done
printf 'FROM %s\nCOPY project/ /project/\n' "$BASE_IMAGE" > "$context/Dockerfile"
docker build -q -t "$OUTPUT_IMAGE" "$context" >/dev/null

remaining=$(diff_against "$OUTPUT_IMAGE")
if [ -n "$remaining" ]; then
    echo "$OUTPUT_IMAGE still differs from the Tapis snapshot:" >&2
    echo "$remaining" >&2
    exit 1
fi
echo "Built $OUTPUT_IMAGE; it matches the Tapis snapshot."
