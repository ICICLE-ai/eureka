"""Contract checks for offline rollout rescoring; no model server is needed."""

import io
import json
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/score-logprobs.py"
SCORER = runpy.run_path(str(SCRIPT))


def response():
    return {"choices": [{
        "prompt_token_ids": [10, 20, 30],
        "prompt_logprobs": [None, {"20": {"logprob": -0.5, "decoded_token": " b"}},
                            {"30": {"logprob": -1.5, "decoded_token": " c"}}],
        "logprobs": {"tokens": ["a", " b", " c"]},
        # Some vLLM versions generate one token despite max_tokens=0. Never score it.
        "token_ids": [99],
    }]}


class ScoreLogprobsTests(unittest.TestCase):
    def test_scores_only_supplied_tokens_and_allows_first_null(self):
        rows = SCORER["token_scores"](response(), [10, 20, 30])
        self.assertEqual([row["token_id"] for row in rows], [10, 20, 30])
        self.assertEqual([row["logprob"] for row in rows], [None, -0.5, -1.5])

    def test_missing_or_misaligned_scores_fail(self):
        for scores in [None, [], [None], [None, None, None],
                       [None, {"99": {"logprob": -1}}, {"30": {"logprob": -1}}]]:
            with self.subTest(scores=scores):
                data = response()
                data["choices"][0]["prompt_logprobs"] = scores
                with self.assertRaises(ValueError):
                    SCORER["token_scores"](data)

    def test_invalid_numeric_scores_fail(self):
        for score in [None, True, "-0.5", float("nan"), float("inf")]:
            with self.subTest(score=score):
                data = response()
                data["choices"][0]["prompt_logprobs"][1]["20"]["logprob"] = score
                with self.assertRaises(ValueError):
                    SCORER["token_scores"](data)

    def test_changed_token_ids_fail(self):
        with self.assertRaisesRegex(ValueError, "changed"):
            SCORER["token_scores"](response(), [10, 21, 30])

    def test_cli_preserves_text_and_saves_provenance_without_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prompt, continuation, output = [root / name for name in ["prompt", "answer", "scores.json"]]
            prompt.write_bytes(b"user\r\nassistant: ")
            continuation.write_bytes("π\n".encode("utf-8"))
            key = root / "key"
            key.write_text("test-secret\n")
            args = [str(SCRIPT), str(prompt), "--continuation", str(continuation), "--output", str(output)]
            env = {"BASE_URL": "http://example.invalid/v1", "MODEL": "test", "API_KEY_FILE": str(key)}
            data = response()
            with patch.dict(os.environ, env, clear=True), patch("sys.argv", args), \
                    patch("urllib.request.urlopen", return_value=io.BytesIO(json.dumps(data).encode())) as http, \
                    patch("sys.stdout", new_callable=io.StringIO):
                SCORER["main"]()
            request = http.call_args.args[0]
            self.assertEqual(request.full_url, "http://example.invalid/v1/completions")
            self.assertEqual(request.get_header("Authorization"), "Bearer test-secret")
            saved = json.loads(output.read_text())
            self.assertEqual(saved["request"]["prompt"], "user\r\nassistant: π\n")
            self.assertEqual(saved["provenance"], "rescored")
            self.assertEqual(saved["response"], data)
            self.assertNotIn("test-secret", output.read_text())

    def test_missing_scores_exit_with_error_without_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prompt, output = root / "ids.json", root / "scores.json"
            prompt.write_text("[10, 20, 30]")
            args = [str(SCRIPT), str(prompt), "--token-ids", "--model", "test", "--output", str(output)]
            data = response()
            data["choices"][0]["prompt_logprobs"] = None
            with patch.dict(os.environ, {}, clear=True), patch("sys.argv", args), \
                    patch("urllib.request.urlopen", return_value=io.BytesIO(json.dumps(data).encode())):
                with self.assertRaisesRegex(SystemExit, "Scoring failed: Missing"):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
