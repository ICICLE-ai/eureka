#!/usr/bin/env python3
r"""Rescore an existing sequence through vLLM. Uses only the Python standard library.

  BASE_URL=http://localhost:8000/v1 MODEL=my-model \
    python3 scripts/score-logprobs.py rendered.txt --output scores.json

The file must contain the full rendered prompt and existing answer, including
chat markers, reasoning and tool text. No chat template or special tokens are
added. --continuation appends a second file verbatim before tokenization. Tokens
can cross that file boundary, so the output scores the whole joined sequence.
With --token-ids, read a JSON array of the original token IDs instead of text.

These are current-model log P(token | preceding tokens), not historical scores
or temperature/top-p adjusted sampling probabilities. The first token has no
preceding context and normally has a null score. Missing scores elsewhere fail.
Credentials come from API_KEY_FILE, DEEPSEEK_API_KEY or OPENAI_API_KEY.
"""

import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import sys
import urllib.error
import urllib.request


def token_scores(response, expected_ids=None):
    """Use prompt scores: some vLLM versions also return one unused output token."""
    choices = response.get("choices")
    if not isinstance(choices, list) or len(choices) != 1:
        raise ValueError("Expected exactly one scored sequence")
    choice = choices[0]
    ids = choice.get("prompt_token_ids")
    scores = choice.get("prompt_logprobs")
    if not isinstance(ids, list) or len(ids) < 2 or any(
        type(token) is not int or token < 0 for token in ids
    ):
        raise ValueError("Missing prompt token IDs or fewer than two tokens to score")
    if expected_ids is not None and ids != expected_ids:
        raise ValueError("Server changed the supplied token IDs; refusing misaligned scores")
    if not isinstance(scores, list) or len(scores) != len(ids):
        raise ValueError("Missing or misaligned prompt logprobs")

    # The echoed tokens supply the first token's text, whose prompt score is null.
    texts = (choice.get("logprobs") or {}).get("tokens", [])
    rows = []
    for index, (token_id, entries) in enumerate(zip(ids, scores)):
        if index == 0 and entries is None:
            score = None
            text = texts[0] if texts else None
        else:
            entry = entries.get(str(token_id)) if isinstance(entries, dict) else None
            score = entry.get("logprob") if isinstance(entry, dict) else None
            if type(score) not in (int, float) or not math.isfinite(score):
                raise ValueError(f"Missing or invalid logprob at token {index} (ID {token_id})")
            text = entry.get("decoded_token")
        rows.append({"index": index, "token_id": token_id, "token": text, "logprob": score})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("text", type=Path, help="UTF-8 rendered sequence, or JSON token IDs")
    parser.add_argument("--continuation", type=Path, help="append existing answer text verbatim")
    parser.add_argument("--token-ids", action="store_true", help="input is a JSON array of token IDs")
    parser.add_argument("--output", required=True, type=Path, help="new JSON file; never overwritten")
    parser.add_argument("--base-url", default=os.environ.get("BASE_URL", "http://localhost:8000/v1"))
    parser.add_argument("--model", default=os.environ.get("MODEL"))
    parser.add_argument("--timeout", type=float, default=600, help="HTTP timeout in seconds (default: 600)")
    args = parser.parse_args()
    if not args.model:
        parser.error("Set MODEL or pass --model")
    if args.token_ids and args.continuation:
        parser.error("--token-ids takes the whole sequence; omit --continuation")
    if args.output.exists():
        parser.error(f"Output already exists: {args.output}")

    # read_bytes preserves CRLF and trailing whitespace; both affect tokenization.
    prompt = args.text.read_bytes().decode("utf-8")
    if args.token_ids:
        prompt = json.loads(prompt)
        if not isinstance(prompt, list) or len(prompt) < 2 or any(
            type(token) is not int or token < 0 for token in prompt
        ):
            parser.error("Token IDs must be a JSON array of at least two nonnegative integers")
    elif args.continuation:
        prompt += args.continuation.read_bytes().decode("utf-8")
    if not prompt:
        parser.error("Input is empty")

    key_file = os.environ.get("API_KEY_FILE")
    key = Path(key_file).read_text().strip() if key_file else (
        os.environ.get("DEEPSEEK_API_KEY") or os.environ.get("OPENAI_API_KEY", "")
    )
    body = {
        "model": args.model, "prompt": prompt, "echo": True, "max_tokens": 0,
        "logprobs": 0, "prompt_logprobs": 0, "return_token_ids": True,
        "add_special_tokens": False, "skip_special_tokens": False, "temperature": 1,
    }
    url = args.base_url.rstrip("/") + "/completions"
    headers = {"Content-Type": "application/json"}
    if key:
        headers["Authorization"] = "Bearer " + key
    request = urllib.request.Request(url, data=json.dumps(body).encode("utf-8"), headers=headers)
    with urllib.request.urlopen(request, timeout=args.timeout) as result:
        response = json.load(result)
    tokens = token_scores(response, prompt if args.token_ids else None)

    # Keep the request and raw response so the new scores have auditable provenance.
    # Headers (including credentials) are deliberately excluded.
    record = {
        "provenance": "rescored", "scored_at": datetime.now(timezone.utc).isoformat(),
        "endpoint": url, "request": body, "response": response, "tokens": tokens,
    }
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(record, output, ensure_ascii=False, allow_nan=False)
        output.write("\n")
    print(f"Saved {sum(t['logprob'] is not None for t in tokens)}/{len(tokens)} token scores to {args.output}")


if __name__ == "__main__":
    try:
        main()
    except urllib.error.HTTPError as error:
        # Providers may include request contents in their error; report only status.
        sys.exit(f"Scoring failed: HTTP {error.code} {error.reason}")
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        sys.exit(f"Scoring failed: {error}")
