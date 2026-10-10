# Retrieval tools

Optional search MCP for Eureka. Requires the Beam pool image and `harness-agents` network.
Run from the repository root:

```sh
docker build --target client -t eureka-agent:local tools/retrieval
docker compose -f tools/retrieval/compose.yaml up -d --build --wait
```

Add these arguments to the usual harness command:

```sh
--image eureka-agent:local --mcp tools/retrieval/mcp.json
```

Tools appear as `mcp_retrieval_lean_local_search`, `mcp_retrieval_lean_loogle`,
`mcp_retrieval_lean_leansearch`, `mcp_retrieval_lean_leanfinder`, and
`mcp_retrieval_lean_hammer_premise`. Hammer takes one goal's text from Beam
`lean_goals` as `query`; it returns suggested lemma names.

Local search reads the attempt's source and packages. It is a heuristic source
search; generated declarations require Beam's `lean_workspace_symbols`.
The shared service searches a prebuilt Loogle index of the image's Mathlib.
It rejects requests from a different Lean/Mathlib snapshot. It never mounts
attempts, proof archives, or the harness store, and never falls back to public Loogle.

LeanSearch, Lean Finder, and Hammer send query/goal text to their public providers.
Those indexes may differ from the project's Mathlib; check returned lemmas with Lean.
Provider outages and rate limits return tool errors, not empty results.
Limits apply across all attempts. Only the service has outbound internet access;
its API accepts fixed search operations, never arbitrary URLs or commands.
It publishes no host port. Keep it on a trusted internal Docker network.

The service owns one Loogle process (allow 12 GB RAM). The first image build
compiles Loogle and indexes Mathlib; subsequent starts read that index.
Changing Beam/Mathlib requires rebuilding both images and extracting matching
packages as described in [pool setup](../../harness/deploy/README.md).
Use `--build-arg BEAM_IMAGE=...` for the client and `BEAM_POOL_IMAGE=...`
for Compose when using a different Beam image.

Stop only retrieval with `docker compose -f tools/retrieval/compose.yaml down`.

Run offline tests with Python 3.12 or later:

```sh
python3 -m venv tools/retrieval/.venv
tools/retrieval/.venv/bin/pip install -r tools/retrieval/requirements.txt
tools/retrieval/.venv/bin/python -m unittest discover -s tools/retrieval/tests -v
```

With the pool and retrieval running, set the normal `HARNESS_PACKAGES` and
`HARNESS_POOL_TOKEN_FILE` test variables, then run:

```sh
HARNESS_RETRIEVAL_IMAGE=eureka-agent:local python3 harness/tests/lifecycle.py Lifecycle.test_retrieval_in_sandbox -v
```

This test uses scripted model responses and local searches; it makes no paid model calls.
Upstream code and provider attribution are recorded in [NOTICE](NOTICE).
