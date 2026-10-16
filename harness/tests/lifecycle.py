#!/usr/bin/env python3
"""Local integration tests: real sandboxes/Beam/Comparator, scripted model responses.
No model provider, API keys, or paid requests are used.

Build the harness, start the Beam pool, then from the repository root:
    export HARNESS_PACKAGES=/path/to/pool-packages
    export HARNESS_POOL_TOKEN_FILE=/path/to/pool.token
    python3 harness/tests/lifecycle.py -v

test_retrieval_in_sandbox also needs HARNESS_RETRIEVAL_IMAGE and the retrieval service.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
PROOF = 'theorem add_comm_nat (a b : Nat) : a + b = b + a := Nat.add_comm a b\n\ntheorem two_mul_eq (n : Nat) : 2 * n = n + n := Nat.two_mul n\n'


def call(name, args):
    return dict(type='function_call', call_id=f'call-{name}', name=name, arguments=json.dumps(args))


class Model(BaseHTTPRequestHandler):
    # Fake /responses endpoint. Each test scripts the replies with server.respond.
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        try:
            if getattr(self.server, 'reject_logprobs', False) and 'top_logprobs' in body:
                self.send_error(400, 'logprobs are not supported by this provider')
                return
            items = self.server.respond(body)
            if items is None:
                self.send_error(400, 'deliberate model failure')
                return
            events = list(getattr(self.server, 'stream_prefix', []))
            if getattr(self.server, 'disconnect_once', False):
                self.server.disconnect_once = False
            else:
                events += [dict(type='response.output_item.done', item=item) for item in items]
                response = dict(usage=dict(input_tokens=11, output_tokens=7))
                response.update(getattr(self.server, 'response_extra', {}))
                events.append(dict(type='response.completed', response=response))
            data = ''.join(f'data: {json.dumps(e)}\n\n' for e in events).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except Exception as error:
            self.server.errors.append(error)
            self.send_error(500)


@unittest.skipUnless(os.getenv('HARNESS_PACKAGES') and os.getenv('HARNESS_POOL_TOKEN_FILE'), 'set pool fixture variables')
class Lifecycle(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='harness-lifecycle-')
        self.dir = Path(self.temp.name)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
        self.server.errors = []
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.args = [str(ROOT/'harness/target/debug/harness'), '--template', str(ROOT/'examples/simple'),
                     '--store', str(self.dir), '--packages', os.environ['HARNESS_PACKAGES'],
                     '--pool-token-file', os.environ['HARNESS_POOL_TOKEN_FILE'], '--comparator', str(ROOT/'comparator'),
                     '--beam-skill', str(ROOT/'lean-beam/skills/lean-beam'), '--trace', str(self.dir/'trace.jsonl'),
                     '--base-url', f'http://127.0.0.1:{self.server.server_port}', '--max-turns', '12', '--logprobs=false']

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temp.cleanup()

    def run_harness(self, *extra):
        # main.rs won't start without a key; the fake server ignores it.
        env = dict(os.environ, DEEPSEEK_API_KEY='local-test-only')
        result = subprocess.run(self.args + list(extra), cwd=ROOT, env=env, text=True, capture_output=True, timeout=150)
        self.assertFalse(self.server.errors, self.server.errors)
        return result

    def records(self):
        return [json.loads(p.read_text()) for p in sorted(self.dir.glob('pool-*/attempts/*/result.json'))]

    def test_default_capture_keeps_reasoning_logprobs_and_interrupted_stream(self):
        self.args.remove('--logprobs=false')
        # Vista returns logprobs only in the terminal response, not item.done.
        # A disconnected first try must also leave its partial reasoning on disk.
        reasoning = dict(type='reasoning', content=[dict(type='reasoning_text', text='saved reasoning')])
        message = dict(type='message', role='assistant', content=[dict(type='output_text', text='done')])
        logprobs = [dict(token='done', logprob=-0.25, bytes=[100, 111, 110, 101], top_logprobs=[])]
        final = dict(message, content=[dict(type='output_text', text='done', logprobs=logprobs)])
        self.server.stream_prefix = [dict(type='response.reasoning_text.delta', delta='partial reasoning')]
        self.server.disconnect_once = True
        self.server.response_extra = dict(output=[reasoning, final], provider_extension={'retained': True})
        def respond(body):
            self.assertIn('message.output_text.logprobs', body['include'])
            self.assertEqual(body['top_logprobs'], 0)
            return [reasoning, message]
        self.server.respond = respond
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)
        row, = self.records()
        folder = Path(row['candidate']).parent
        response = json.loads((folder/'response-0001.json').read_text())
        self.assertEqual(response['items'][0], reasoning)
        self.assertEqual(response['response']['output'][1]['content'][0]['logprobs'], logprobs)
        self.assertTrue(response['response']['provider_extension']['retained'])
        stream = [json.loads(line) for line in (folder/'stream-0001.jsonl').read_text().splitlines()]
        self.assertEqual([r['try'] for r in stream if r['event'] == 'request'], [1, 2])
        self.assertTrue(any(r['try'] == 1 and 'partial reasoning' in r.get('data', '') for r in stream))
        for path in folder.glob('*.json*'):
            self.assertNotIn('local-test-only', path.read_text(), str(path))

    def test_unsupported_logprobs_are_fatal_and_captured(self):
        self.args.remove('--logprobs=false')
        self.server.reject_logprobs = True
        self.server.respond = lambda _: self.fail('unexpected fallback without logprobs')
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)
        row, = self.records()
        folder = Path(row['candidate']).parent
        stream = [json.loads(line) for line in (folder/'stream-0001.jsonl').read_text().splitlines()]
        requests = [r['body'] for r in stream if r['event'] == 'request']
        self.assertEqual(row['status'], 'failed')
        self.assertEqual(len(requests), 1)
        self.assertIn('top_logprobs', requests[0])
        self.assertTrue(any(r['event']=='http_error' and r['status']==400 for r in stream))
        self.assertFalse((folder/'request-0002.json').exists())

    def test_missing_logprobs_fail_before_tools_and_keep_response(self):
        self.args.remove('--logprobs=false')
        self.server.respond = lambda _: [call('write_file', {'path': 'Proofs/Solution.lean', 'content': PROOF})]
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)
        row, = self.records()
        self.assertEqual(row['status'], 'failed')
        self.assertEqual(row['usage']['output_tokens'], 7)
        folder = Path(row['candidate']).parent
        self.assertTrue((folder/'response-0001.json').is_file())
        self.assertEqual((folder/'candidate.lean').read_bytes(), (ROOT/'examples/simple/Proofs/Solution.lean').read_bytes())
        trace = [json.loads(line) for line in (self.dir/'trace.jsonl').read_text().splitlines()]
        self.assertTrue(any(e['event']=='agent_error' and 'logprobs requested' in e['error'] for e in trace))
        self.assertFalse(any(e['event']=='tool_start' for e in trace))

    def test_logprobs_opt_out_allows_unscored_tools(self):
        # This fixture explicitly passes --logprobs=false. Missing scores must
        # not prevent the tool from running or the next model turn from starting.
        calls = 0
        def respond(body):
            nonlocal calls
            calls += 1
            self.assertNotIn('top_logprobs', body)
            self.assertNotIn('message.output_text.logprobs', body['include'])
            if calls == 1:
                return [call('write_file', {'path': 'Proofs/Solution.lean', 'content': PROOF})]
            return [dict(type='message', role='assistant', content=[dict(type='output_text', text='done')])]
        self.server.respond = respond
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)  # No proof was submitted.
        row, = self.records()
        self.assertEqual(calls, 2)
        self.assertEqual(row['status'], 'not_submitted')
        self.assertEqual(row['termination'], 'no_tool_calls')
        self.assertEqual(Path(row['candidate']).read_text(), PROOF)

    def test_children_order_notes_and_cancellation(self):
        # The lead's first turn reads, writes and reads again, to check call order.
        # One child finishes with a note; the other is still sleeping when the lead
        # stops without submitting and gets cancelled. All three save candidates.
        turns = {}
        observed = []
        def respond(body):
            ident = body['prompt_cache_key']
            turn = turns.get(ident, 0)
            turns[ident] = turn + 1
            if ident == 'attempt-000002':
                return [call('finish', {'note': 'retained child note; partial proof'})]
            if ident == 'attempt-000003':
                return [call('shell', {'command': 'sleep 300'})]
            if turn == 0:
                return [call('read_file', {'path': 'Proofs/Solution.lean'}),
                        call('write_file', {'path': 'Proofs/Solution.lean', 'content': PROOF}),
                        call('read_file', {'path': 'Proofs/Solution.lean'}),
                        call('spawn_subagents', {'tasks': [dict(label='same-label', instructions='finish immediately')]})]
            outputs = [i['output'] for i in body['input'] if i.get('type') == 'function_call_output']
            observed[:] = outputs
            finished = any('retained child note' in o for o in outputs)
            spawned_second = any('attempt-000003' in o for o in outputs)
            if not finished:
                return [call('wait_subagents', {})]
            if not spawned_second:
                return [call('spawn_subagents', {'tasks': [dict(label='same-label', instructions='sleep')]}), call('shell', {'command': 'sleep 3'})]
            return [dict(type='message', role='assistant', content=[dict(type='output_text', text='partial')])]
        self.server.respond = respond
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)
        rows = self.records()
        self.assertEqual(len(rows), 3, result.stderr)
        self.assertEqual(len({r['id'] for r in rows}), 3)
        self.assertIn('sorry', observed[0])
        self.assertNotIn('sorry', observed[2])
        self.assertEqual(rows[1]['note'], 'retained child note; partial proof')
        self.assertEqual(rows[2]['termination'], 'cancelled')
        for row in rows:
            self.assertEqual(Path(row['candidate']).read_text(), PROOF)
        trace = [json.loads(line) for line in (self.dir/'trace.jsonl').read_text().splitlines()]
        ended = [e for e in trace if e['event'] == 'attempt_end']
        self.assertEqual(ended[-1]['id'], 'attempt-000001', 'parent ended before its children')
        run_id = next(self.dir.glob('pool-*')).name
        remaining = subprocess.check_output(['docker', 'ps', '-aq', '--filter', f'label=harness.run={run_id}'], text=True)
        self.assertFalse(remaining.strip(), 'orphan containers')

    def test_model_failure_keeps_usage_and_candidate(self):
        # The second request fails with a 400. Usage and the candidate must survive.
        turns = 0
        def respond(_):
            nonlocal turns
            turns += 1
            return [call('read_file', {'path': 'Proofs/Solution.lean'})] if turns == 1 else None
        self.server.respond = respond
        result = self.run_harness()
        self.assertEqual(result.returncode, 2, result.stderr)
        row, = self.records()
        self.assertEqual(row['status'], 'failed')
        self.assertEqual(row['usage']['input_tokens'], 11)
        self.assertEqual(row['usage']['output_tokens'], 7)
        self.assertTrue(Path(row['candidate']).is_file())

    def test_failed_verification_is_recoverable(self):
        # The template still has sorries, so this fails, but the candidate and
        # report must still be saved.
        self.server.respond = lambda _: self.fail('verify-only called the model')
        result = self.run_harness('--verify-only', str(ROOT/'examples/simple/Proofs/Solution.lean'))
        self.assertNotEqual(result.returncode, 0)
        row, = self.records()
        self.assertIn(row['status'], ('rejected', 'failed'))
        candidate = Path(row['candidate'])
        self.assertEqual(candidate.read_bytes(), (ROOT/'examples/simple/Proofs/Solution.lean').read_bytes())
        self.assertTrue(candidate.with_name('verification.txt').stat().st_size)

    @unittest.skipUnless(os.getenv('HARNESS_RETRIEVAL_IMAGE'), 'set HARNESS_RETRIEVAL_IMAGE and start retrieval')
    def test_retrieval_in_sandbox(self):
        # Local search and Loogle, including a bad query followed by a good one.
        # The hosted tools are only checked for presence.
        turns = 0
        observed = []
        def respond(body):
            nonlocal turns
            turns += 1
            if turns == 1:
                names = {t['name'] for t in body['tools']}
                self.assertTrue({f'mcp_retrieval_{name}' for name in (
                    'lean_local_search', 'lean_loogle', 'lean_leansearch',
                    'lean_leanfinder', 'lean_hammer_premise')} <= names)
                return [call('mcp_retrieval_lean_loogle', {'query': '"Nat.add_comm"', 'num_results': 2}),
                        call('mcp_retrieval_lean_local_search', {'query': 'add_comm_nat'}),
                        call('mcp_retrieval_lean_loogle', {'query': '"unterminated'}),
                        call('mcp_retrieval_lean_loogle', {'query': '"Nat.mul_comm"', 'num_results': 2})]
            observed[:] = [i['output'] for i in body['input'] if i.get('type') == 'function_call_output']
            return [dict(type='message', role='assistant', content=[dict(type='output_text', text='search smoke complete')])]
        self.server.respond = respond
        result = self.run_harness('--image', os.environ['HARNESS_RETRIEVAL_IMAGE'],
                                  '--mcp', str(ROOT/'tools/retrieval/mcp.json'))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(len(observed), 4, result.stderr)
        self.assertIn('Nat.add_comm', observed[0])
        self.assertIn('local-loogle', observed[0])
        self.assertIn('add_comm_nat', observed[1])
        self.assertTrue(observed[2].startswith('ERROR:'), observed[2])
        self.assertIn('Nat.mul_comm', observed[3])
        record, = self.records()
        self.assertEqual(Path(record['candidate']).read_bytes(), (ROOT/'examples/simple/Proofs/Solution.lean').read_bytes())

    def test_extra_mcp_preserves_its_schema_and_arguments(self):
        # Only Beam gets `workspace` injected; other servers see it untouched.
        server_code = """import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'echo', 'inputSchema': {'type': 'object', 'properties': {'workspace': {'type': 'string'}}, 'required': ['workspace']}}]}
    elif method == 'tools/call':
        result = {'content': [{'type': 'text', 'text': request['params']['arguments']['workspace']}]}
    else: result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"""
        config = self.dir/'mcp.json'
        config.write_text(json.dumps(dict(name='fixture', command='python3', args=['-u', '-c', server_code])))
        def respond(body):
            tool = next(t for t in body['tools'] if t['name'] == 'mcp_fixture_echo')
            self.assertIn('workspace', tool['parameters']['properties'])
            return [call('mcp_fixture_echo', {'workspace': 'preserved'})]
        self.server.respond = respond
        self.args[self.args.index('--max-turns') + 1] = '1'
        result = self.run_harness('--mcp', str(config))
        self.assertEqual(result.returncode, 2, result.stderr)
        events = [json.loads(line) for line in (self.dir/'trace.jsonl').read_text().splitlines()]
        output, = [e['output'] for e in events if e['event'] == 'tool']
        self.assertEqual(output, 'preserved')

    def test_saved_proof_can_be_reverified_and_published(self):
        self.server.respond = lambda _: self.fail('verify-only called the model')
        candidate = self.dir/'seed.lean'
        candidate.write_text(PROOF)
        result = self.run_harness('--verify-only', str(candidate))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('VERIFIED', result.stdout)
        row, = self.records()
        self.assertEqual(row['status'], 'verified')
        winners = list(self.dir.glob('pool-*/results/**/winner/project/Proofs/Solution.lean'))
        self.assertEqual(len(winners), 1)
        self.assertEqual(winners[0].read_text(), PROOF)


if __name__ == '__main__':
    unittest.main()
