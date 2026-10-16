#!/usr/bin/env bash
# Stop a run. The agents save their candidates first, which can take a moment.
#
#   scripts/stop.sh runs/mine
set -euo pipefail

run=${1:?usage: scripts/stop.sh runs/mine}
python3 - "$run" <<'PY'
import os
from pathlib import Path
import select
import signal
import sys

run = Path(sys.argv[1]).resolve()
pid = int((run / "pid").read_text().strip())
if pid <= 1:
    sys.exit(f"invalid PID in {run / 'pid'}")
try:
    # A pidfd keeps the signal tied to this process, even if its PID is reused.
    fd = os.pidfd_open(pid)
    proc = Path(f"/proc/{pid}")
    args = [os.fsdecode(arg) for arg in (proc / "cmdline").read_bytes().split(b'\0') if arg]
    if not args:  # Already exited; an unreaped zombie still has a PID.
        sys.exit(0)
    stores = [value for flag, value in zip(args, args[1:]) if flag == '--store']
    if Path(args[0]).name != 'harness' or not any(
        (proc / 'cwd' / store).resolve() == run / 'store' for store in stores
    ):
        sys.exit(f"PID {pid} does not belong to this run; refusing to stop it")
    signal.pidfd_send_signal(fd, signal.SIGTERM)
    print(f"Stopping {pid}...", flush=True)
    poll = select.poll()
    poll.register(fd, select.POLLIN)
    poll.poll()  # Ready on exit, including a zombie awaiting its parent's reap.
except (ProcessLookupError, FileNotFoundError):
    print(f"PID {pid} is not running")
PY
tail -n 5 "$run/run.log"
