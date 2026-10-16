#!/usr/bin/env bash
# Build Comparator and the harness.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$ROOT"

for module in comparator lean-beam; do
    if [ ! -e "$module/.git" ]; then
        echo "$module is missing; run: git submodule update --init --recursive" >&2
        exit 1
    fi
done
(cd comparator && lake build lean4export comparator)
cargo build --locked --manifest-path harness/Cargo.toml
