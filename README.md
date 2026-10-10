# Eureka

In development.

Clone with `--recurse-submodules`. Set up the [Beam pool](harness/deploy/README.md) and [Comparator](comparator/README.md).
Copy `examples/simple/` to your own problem directory. Keep its build files.
Put your theorem statements in both `Proofs/Challenge.lean` and `Proofs/Solution.lean`, with `sorry` proofs.
List their full names in `config.json` under `theorem_names`.
Set `DEEPSEEK_API_KEY` in your environment. Run from the repository root:

```bash
(cd comparator && lake build lean4export comparator)
mkdir -p runs/mine/store
cargo run --locked --manifest-path harness/Cargo.toml -- \
  --template /path/to/your/problem --store runs/mine/store \
  --packages /path/to/pool-packages --pool-token-file /path/to/pool.token \
  --trace runs/mine/trace.jsonl
```
