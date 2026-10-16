"""Print the project files that differ from the snapshot the Tapis workers serve.

Runs inside a sandbox image (see build-tapis-image.sh), on harness-agents with the
bridge up and BEAM_POOL_TOKEN set. Prints one path per line; nothing means a match.
"""
import asyncio
import os
from pathlib import Path

from scripts.beam_pool.protocol import Snapshot
from scripts.beam_pool.transport import rpc


async def main():
    endpoint, token = ("tapis-gateway", 9000), os.environ["BEAM_POOL_TOKEN"]
    local = Snapshot.create(Path("/project")).files
    info = await rpc(endpoint, token, "info", {})
    snapshots = {w["snapshot"] for w in info["workers"] if w.get("ready")}
    if len(snapshots) != 1:
        raise SystemExit(f"expected one snapshot among ready workers, found {len(snapshots)}")
    snapshot = snapshots.pop()
    remote, offset = {}, 0
    while offset is not None:
        page = await rpc(endpoint, token, "describe", {"snapshot": snapshot, "offset": offset})
        remote.update(page["files"])
        offset = page["next"]
    for path in sorted(remote.keys() | local.keys()):
        if remote.get(path) != local.get(path):
            print(path)


asyncio.run(main())
