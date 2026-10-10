# Beam pool for the harness

The harness runs every attempt in a Docker sandbox built from Beam's pool image
and attached to a Beam pool (gateway + workers) over an internal-only network.

Build the image (Lean 4.33 + prepared Mathlib + Beam) from the lean-beam checkout:

    docker build -f lean-beam/deploy/beam-pool/Dockerfile -t beam-pool:local lean-beam

Extract the image's prepared packages once; every sandbox mounts them read-only:

    docker create --name pool-extract beam-pool:local
    docker cp pool-extract:/project/.lake/packages /path/to/pool-packages
    docker rm pool-extract

Start the pool. `pool-override.yaml` adds the gateway to the internal network
`harness-agents` that sandboxes use; workers stay on Compose's internal network.

    (umask 077; python3 -c 'import secrets; print(secrets.token_hex(32))' > /path/to/pool.token)
    BEAM_POOL_TOKEN=$(cat /path/to/pool.token) docker compose \
        -f lean-beam/deploy/beam-pool/compose.yaml -f harness/deploy/pool-override.yaml \
        up -d --scale worker=4

Then run the harness with `--packages /path/to/pool-packages --pool-token-file /path/to/pool.token`.
Problem templates use the image's layout: its `lakefile.toml`, `lake-manifest.json` and
`lean-toolchain`, an empty root `Proofs.lean`, `Proofs/Challenge.lean`, `Proofs/Solution.lean`,
and `config.json` naming modules `Proofs.Challenge` and `Proofs.Solution`.

## Running, stopping, and recovery

The per-turn output cap defaults to 32,768 tokens, including reasoning; use
`--max-output-tokens` to override it. Tool calls execute in model order.
`lean_check {commands: [...]}` still probes concurrently.

`spawn_subagents` returns unique attempt IDs immediately. `wait_subagents {}`
waits up to 30 seconds and delivers newly completed results once, including
`finish.note` and the final target text. A lead can have 8 uncollected children.
The seed must elaborate before spawning; sorries are allowed. Parent completion,
SIGINT, SIGTERM, or a published winner cancels remaining agents and joins their
cleanup. Use SIGTERM for a graceful stop; SIGKILL cannot run cleanup.

Each attempt saves under `<pool>/attempts/<id>/` outside all sandbox mounts:

- `candidate.lean`: frozen target bytes, captured after stopping the sandbox.
- `result.json`: label, status, termination, notes, token usage and candidate path.
- `verification.txt`: verifier reports, including both tries after an operational
  failure. Proof rejections are not retried; only Comparator can verify a proof.

Restart with the same trusted template and `--seed /path/to/candidate.lean`.
Use `--verify-only /path/to/candidate.lean` with the usual template/store/packages/
Comparator arguments to reverify without a model. This creates a new pool and
preserves the old bundle. It does not resume the conversation.

`--capture-requests` adds each exact request JSON body and parsed response beside
its attempt. Authorization headers are never saved. Capture is opt-in because
request histories can be large. Events always include model/tool starts, finishes,
retries and errors. Inspect either old or new traces without changing them:

    python3 harness/trace_report.py /path/to/trace.jsonl
    python3 harness/trace_report.py /path/to/trace.jsonl --json

Additional stdio MCP servers can be configured with repeated `--mcp server.json`:

    {"name": "retrieval", "command": "python3", "args": ["/opt/retrieval/server.py"]}

The command and its files must be available in the sandbox image; it shares the
sandbox's filesystem and network restrictions. Tool names are prefixed with
`mcp_<name>_`. For local Loogle and hosted lemma search, see the optional
[retrieval tools](../../tools/retrieval/README.md).

For local integration tests (real Docker/Beam/Comparator, scripted responses,
no provider calls), build first, then run:

    HARNESS_PACKAGES=/path/to/pool-packages \
    HARNESS_POOL_TOKEN_FILE=/path/to/pool.token \
    python3 harness/tests/lifecycle.py -v
