"""Stdio MCP for one attempt; only the shared retrieval service needs internet."""

import argparse
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

import httpx
from backends import Count, Query, SearchRequest, Version, project_snapshot
from local import local_search
from mcp.server.fastmcp import FastMCP
from mcp.server.fastmcp.exceptions import ToolError
from mcp.types import ToolAnnotations


def create_server(endpoint: str, project: Path) -> FastMCP:
    http = httpx.AsyncClient(
        base_url=endpoint.rstrip("/"), timeout=45, follow_redirects=False, trust_env=False
    )

    @asynccontextmanager
    async def lifespan(_):
        async with http:
            yield {}

    server = FastMCP("Eureka retrieval", lifespan=lifespan)
    annotations = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=True
    )

    async def search(provider, query, num_results, version="v4.28.0"):
        try:
            request = SearchRequest(
                provider=provider,
                query=query,
                num_results=num_results,
                version=version,
                snapshot=project_snapshot(project) if provider == "loogle" else None,
            )
            response = await http.post("/search", json=request.model_dump())
            if response.is_error:
                raise ToolError(response.json().get("error", "Retrieval service failed"))
            response.raise_for_status()
            return response.json()
        except (httpx.HTTPError, OSError, ValueError, KeyError, StopIteration) as error:
            raise ToolError(f"Retrieval unavailable: {error}") from error

    @server.tool(
        name="lean_local_search",
        annotations=ToolAnnotations(
            readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False
        ),
    )
    async def local(query: Query, limit: Count = 10) -> dict[str, Any]:
        """Find source declarations in this project and its packages. Generated declarations may be absent; use Beam lean_workspace_symbols for those. An empty result is not proof of absence."""
        return await local_search(project, query, limit)

    @server.tool(name="lean_loogle", annotations=annotations)
    async def loogle(query: Query, num_results: Count = 8) -> dict[str, Any]:
        """Search the project's Mathlib snapshot by type pattern, constant or quoted name substring. Local Loogle; no public-service fallback."""
        return await search("loogle", query, num_results)

    @server.tool(name="lean_leansearch", annotations=annotations)
    async def leansearch(query: Query, num_results: Count = 5) -> dict[str, Any]:
        """Search Mathlib via leansearch.net with natural language or a Lean term. Check suggested declarations locally before using them."""
        return await search("leansearch", query, num_results)

    @server.tool(name="lean_leanfinder", annotations=annotations)
    async def leanfinder(
        query: Query, num_results: Count = 5, version: Version = "v4.28.0"
    ) -> dict[str, Any]:
        """Semantic search by mathematical meaning or proof state. The selected hosted Mathlib snapshot may differ from this project's; check results locally."""
        return await search("leanfinder", query, num_results, version)

    @server.tool(name="lean_hammer_premise", annotations=annotations)
    async def hammer(query: Query, num_results: Count = 32) -> dict[str, Any]:
        """Suggest lemma names for a goal. Pass one goal's text from Beam lean_goals as query. Uses a hosted premise retriever; does not run Hammer or prove the goal."""
        return await search("hammer", query, num_results)

    return server


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", default="http://retrieval:8000")
    parser.add_argument("--project", type=Path, default=Path.cwd())
    args = parser.parse_args()
    create_server(args.endpoint, args.project.resolve()).run()
