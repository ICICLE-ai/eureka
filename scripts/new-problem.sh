#!/usr/bin/env bash
# Create a problem directory from examples/simple.
#
#   scripts/new-problem.sh Problem.lean runs/problem [MyProblem.main_theorem ...]
#
# Problem.lean holds the imports, definitions and theorem statements, with
# `sorry` proofs. It becomes both Challenge.lean and the starting Solution.lean.
# The theorem names go into config.json; you can also set them there by hand.
set -euo pipefail
source "$(dirname "$0")/env.sh"

if [ $# -lt 2 ]; then
    sed -n '4p' "$0" | sed 's/^#  *//' >&2
    exit 1
fi
lean=$(realpath -- "$1")
dir=$(realpath -m -- "$2")
shift 2
if [ -e "$dir" ]; then
    echo "$dir already exists" >&2
    exit 1
fi

template=$ROOT/examples/simple
mkdir -p "$dir/Proofs"
cp "$template"/{lakefile.toml,lake-manifest.json,lean-toolchain,Proofs.lean,config.json} "$dir/"
cp "$lean" "$dir/Proofs/Challenge.lean"
cp "$lean" "$dir/Proofs/Solution.lean"

if [ $# -gt 0 ]; then
    python3 - "$dir/config.json" "$@" <<'PY'
import json, sys
path, names = sys.argv[1], sys.argv[2:]
with open(path) as f:
    config = json.load(f)
config['theorem_names'] = names
with open(path, 'w') as f:
    json.dump(config, f, indent=4)
    f.write('\n')
PY
else
    echo "Now set theorem_names in $dir/config.json."
fi
echo "Review $dir/Proofs/Challenge.lean: those statements are what the verifier checks."
