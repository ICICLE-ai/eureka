"""Fixed search providers; adapted from lean-lsp-mcp (see NOTICE and LICENSE)."""

import asyncio
import json
import os
import signal
import time
from collections import deque
from pathlib import Path
from typing import Annotated, ClassVar, Literal

import httpx
from pydantic import BaseModel, ConfigDict, Field, StringConstraints

Query = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1, max_length=8192)]
Count = Annotated[int, Field(strict=True, ge=1, le=32)]
Version = Literal["v4.19.0", "v4.24.0", "v4.28.0"]
Provider = Literal["loogle", "leansearch", "leanfinder", "hammer"]
MAX_RESPONSE = 2 * 1024 * 1024


class SearchError(Exception):
    pass


class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    provider: Provider
    query: Query
    num_results: Count = 5
    version: Version = "v4.28.0"
    snapshot: dict[str, str] | None = None


def project_snapshot(project: Path) -> dict[str, str]:
    manifest = json.loads((project / "lake-manifest.json").read_text())
    return {
        "lean_toolchain": (project / "lean-toolchain").read_text().strip(),
        "mathlib_revision": next(p["rev"] for p in manifest["packages"] if p["name"] == "mathlib"),
    }


class Loogle:
    """One serialized interactive process. A failed read always resets its stream."""

    def __init__(self, project: Path, binary: str, index: str):
        self.project = project
        self.command = [
            "lake",
            "env",
            binary,
            "--json",
            "--interactive",
            "--index-mode",
            "read",
            "--index-file",
            index,
            "--max-results",
            "32",
        ]
        self.process = None
        self.lock = asyncio.Lock()

    async def start(self):
        if self.process is not None and self.process.returncode is None:
            return
        await self.close()
        self.process = await asyncio.create_subprocess_exec(
            *self.command,
            cwd=self.project,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            limit=MAX_RESPONSE,
            start_new_session=True,
        )
        try:
            ready = await asyncio.wait_for(self.process.stdout.readline(), 180)
            if b"Loogle is ready." not in ready:
                raise SearchError("Loogle failed to load its prepared index; see service logs")
        except BaseException:
            await self.close()
            raise

    async def query(self, query: str, limit: int) -> list[dict]:
        async with self.lock:
            await self.start()
            try:
                async with asyncio.timeout(30):
                    self.process.stdin.write((" ".join(query.splitlines()) + "\n").encode())
                    await self.process.stdin.drain()
                    line = await self.process.stdout.readline()
                    response = json.loads(line)
                    if not isinstance(response, dict):
                        raise TypeError("expected an object")
            except BaseException:
                # A late response must never become the next caller's result.
                await self.close()
                raise
            if error := response.get("error"):
                raise SearchError(f"Loogle query error: {error}")
            return [
                {key: hit.get(key, "") for key in ("name", "type", "module")}
                for hit in response.get("hits", [])[:limit]
            ]

    async def close(self):
        process, self.process = self.process, None
        if process is not None:
            # `lake env` is a parent process, not an exec. Reap its whole group.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            await process.wait()


class Backends:
    # Shared across all attempts, matching the upstream public-service budgets.
    LIMITS: ClassVar = {"leansearch": (90, 30), "leanfinder": (10, 30), "hammer": (6, 30)}
    URLS: ClassVar = {
        "leansearch": "https://leansearch.net/search",
        "leanfinder": "https://lean-lsp-proxy.leanfinder.workers.dev",
        "hammer": "http://leanpremise.net/retrieve",
    }

    def __init__(self, loogle: Loogle, snapshot: dict[str, str], http: httpx.AsyncClient):
        self.loogle, self.snapshot, self.http = loogle, snapshot, http
        self.history = {name: deque() for name in self.LIMITS}

    def rate_limit(self, provider: str):
        limit, period = self.LIMITS[provider]
        history = self.history[provider]
        now = time.monotonic()
        while history and history[0] <= now - period:
            history.popleft()
        if len(history) >= limit:
            raise SearchError(f"{provider}: shared limit of {limit} requests per {period}s; try later")
        history.append(now)

    async def search(self, request: SearchRequest) -> dict:
        provider, query, count = request.provider, request.query, request.num_results
        if provider == "loogle":
            if request.snapshot != self.snapshot:
                raise SearchError(
                    "Loogle snapshot differs from this project's Lean/Mathlib; rebuild the retrieval image"
                )
            items = await self.loogle.query(query, count)
            return {"items": items, "source": "local-loogle", "snapshot": self.snapshot}

        self.rate_limit(provider)
        if provider == "leansearch":
            payload = {"query": [query], "num_results": count}
        elif provider == "leanfinder":
            payload = {"inputs": query, "top_k": count, "version": request.version}
        else:
            payload = {"state": query, "new_premises": [], "k": count}
        async with (
            asyncio.timeout(15),
            self.http.stream("POST", self.URLS[provider], json=payload) as response,
        ):
            response.raise_for_status()
            body = bytearray()
            async for chunk in response.aiter_bytes():
                body.extend(chunk)
                if len(body) > MAX_RESPONSE:
                    raise SearchError(f"{provider}: response too large")
        data = json.loads(body)
        if isinstance(data, dict) and data.get("error"):
            raise SearchError(f"{provider}: {str(data['error'])[:500]}")
        if provider == "leansearch":
            if not isinstance(data, list) or (data and not isinstance(data[0], list)):
                raise SearchError("leansearch: malformed response")
            items = [
                {
                    "name": ".".join(r["result"]["name"]),
                    "module": ".".join(r["result"]["module_name"]),
                    "type": r["result"].get("type"),
                    "kind": r["result"].get("kind"),
                }
                for r in (data[0] if data else [])[:count]
            ]
        elif provider == "leanfinder":
            items = [
                {
                    key: r.get(key, "")
                    for key in (
                        "formal_name",
                        "informal_name",
                        "kind",
                        "type",
                        "informal_description",
                        "path",
                    )
                }
                for r in data["results"][:count]
            ]
        else:
            items = [{"name": r["name"]} for r in data[:count]]
        result = {"items": items, "source": provider}
        if provider == "leanfinder":
            result["version"] = request.version
        return result
