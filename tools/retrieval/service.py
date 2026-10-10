"""Internal-only search gateway. No arbitrary URLs, commands, or project mounts."""

import argparse
import logging
from contextlib import asynccontextmanager
from pathlib import Path

import httpx
import uvicorn
from backends import Backends, Loogle, SearchError, SearchRequest, project_snapshot
from pydantic import ValidationError
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse
from starlette.routing import Route

logger = logging.getLogger("retrieval")


def create_app(backends: Backends, lifespan=None) -> Starlette:
    async def search(request: Request):
        body = bytearray()
        async for chunk in request.stream():
            body.extend(chunk)
            if len(body) > 65536:
                return JSONResponse({"error": "Request too large"}, status_code=413)
        try:
            query = SearchRequest.model_validate_json(body)
        except ValidationError:
            return JSONResponse({"error": "Invalid search arguments"}, status_code=400)
        try:
            return JSONResponse(await backends.search(query))
        except SearchError as error:
            return JSONResponse({"error": str(error)}, status_code=422)
        except httpx.HTTPStatusError as error:
            return JSONResponse(
                {"error": f"{query.provider} returned HTTP {error.response.status_code}; try later"},
                status_code=502,
            )
        except (
            httpx.HTTPError,
            TimeoutError,
            OSError,
            ValueError,
            KeyError,
            TypeError,
            IndexError,
            AttributeError,
        ):
            logger.exception("Search provider %s failed", query.provider)
            return JSONResponse({"error": f"{query.provider} unavailable; see service logs"}, status_code=502)

    async def health(_):
        return JSONResponse({"snapshot": backends.snapshot})

    return Starlette(
        routes=[Route("/search", search, methods=["POST"]), Route("/health", health)], lifespan=lifespan
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=Path("/project"))
    parser.add_argument("--loogle", default="/opt/loogle/.lake/build/bin/loogle")
    parser.add_argument("--index", default="/opt/retrieval/mathlib.idx")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()
    loogle = Loogle(args.project, args.loogle, args.index)
    http = httpx.AsyncClient(
        timeout=10,
        follow_redirects=False,
        trust_env=False,
        limits=httpx.Limits(max_connections=8),
        headers={"User-Agent": "eureka-retrieval/1"},
    )
    backends = Backends(loogle, project_snapshot(args.project), http)

    @asynccontextmanager
    async def lifespan(_):
        try:
            await loogle.start()
            yield
        finally:
            await loogle.close()
            await http.aclose()

    uvicorn.run(
        create_app(backends, lifespan), host=args.host, port=args.port, access_log=False, limit_concurrency=32
    )


if __name__ == "__main__":
    main()
