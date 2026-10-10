"""Bounded source declaration search, adapted from lean-lsp-mcp (see NOTICE)."""

import asyncio
import json
import re
from pathlib import Path

LEAD = (
    r"^\s*(?:@\[[^\]]*\]\s*)*(?:(?:public|protected|private|noncomputable|partial|unsafe|scoped|local)\s+)*"
)
KINDS = r"theorem|lemma|def|axiom|class|instance|structure|inductive|abbrev|opaque"
DECL = re.compile(LEAD + rf"(?P<kind>{KINDS})\s+(?P<name>[\w'.]+)")


def qualify(path: Path, wanted: set[int]) -> dict[int, str]:
    scopes, names = [], {}
    with path.open(errors="replace") as source:
        for line_number, line in enumerate(source, 1):
            if line_number > max(wanted):
                break
            stripped = line.strip()
            if match := re.match(r"namespace\s+([\w.']+)", stripped):
                scopes.append(match[1])
            elif re.match(r"(?:section|mutual)\b", stripped):
                scopes.append("")
            elif re.match(r"end\b", stripped) and scopes:
                scopes.pop()
            if line_number in wanted:
                names[line_number] = ".".join(s for s in scopes if s)
    return names


async def local_search(root: Path, query: str, limit: int) -> dict:
    pattern = LEAD + rf"(?:{KINDS})\s+[\w'.]*{re.escape(query.rsplit('.', 1)[-1])}[\w'.]*(?:\s|:|$)"
    command = [
        "rg",
        "--json",
        "--hidden",
        "--no-ignore",
        "--ignore-case",
        "--color",
        "never",
        "-g",
        "*.lean",
        "-g",
        "!.git/**",
        "-g",
        "!.lake/build/**",
        "-g",
        "!.beam/**",
        "--",
        pattern,
        ".",
    ]
    process = await asyncio.create_subprocess_exec(
        *command,
        cwd=root,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        limit=1024 * 1024,
    )
    matches, truncated = [], False
    try:
        async with asyncio.timeout(10):
            while line := await process.stdout.readline():
                event = json.loads(line)
                if event["type"] != "match":
                    continue
                data = event["data"]
                declaration = DECL.match(data["lines"]["text"])
                if declaration is None:
                    continue
                path = (root / data["path"]["text"]).resolve()
                if not path.is_relative_to(root):
                    continue
                matches.append(
                    {
                        "name": declaration["name"],
                        "kind": declaration["kind"],
                        "file": str(path.relative_to(root)),
                        "line": data["line_number"],
                    }
                )
                if len(matches) >= 2048:
                    truncated = True
                    break
            if not truncated and await process.wait() not in (0, 1):
                raise RuntimeError("Local declaration search failed")
    finally:
        if process.returncode is None:
            process.kill()
        await process.wait()
    for file in {m["file"] for m in matches}:
        selected = [m for m in matches if m["file"] == file]
        prefixes = await asyncio.to_thread(qualify, root / file, {m["line"] for m in selected})
        for match in selected:
            if prefix := prefixes[match["line"]]:
                match["name"] = prefix + "." + match["name"]
    query = query.casefold()
    matches = [m for m in matches if query in m["name"].casefold()]
    matches.sort(
        key=lambda m: (
            m["name"].casefold() != query,
            m["name"].rsplit(".", 1)[-1].casefold() != query,
            m["file"].startswith(".lake/"),
            len(m["name"]),
            m["name"],
        )
    )
    return {
        "items": matches[:limit],
        "source": "local-source",
        "index": "unavailable",
        "truncated": truncated or len(matches) > limit,
    }
