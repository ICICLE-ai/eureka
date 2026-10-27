"""Reader regressions: python3 -m unittest discover -s harness/tests -p 'test_*.py'.

These use temporary traces. No Docker, model, or running proof files are touched.
"""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / 'trace_report.py'
spec = importlib.util.spec_from_file_location('trace_report', SCRIPT)
reporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reporter)


class TraceReaderTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.path = Path(temp.name) / 'trace.jsonl'
        self.reader = reporter.TraceReader(self.path)

    def event(self, kind, **fields):
        return json.dumps(dict(event=kind, attempt='attempt-000001', ts_ms=1000, **fields)) + '\n'

    def test_partial_append_is_counted_once_when_complete(self):
        line = self.event('model', turn=1, usage={'input_tokens': 100, 'cached_input_tokens': 80})
        split = len(line) // 2
        self.path.write_text(line[:split])
        for _ in range(2):
            report = self.reader.read()
            self.assertEqual(report['attempts'], [])
            self.assertEqual(report['malformed_lines'], 1)
            self.assertEqual(self.reader.offset, 0)
        with self.path.open('a') as stream:
            stream.write(line[split:] + self.event('tool', name='lean_sync'))
        for _ in range(2):
            report = self.reader.read()
            self.assertEqual(report['attempts'][0]['input'], 100)
            self.assertEqual(report['attempts'][0]['cached'], 80)
            self.assertEqual(report['attempts'][0]['tool_calls'], 1)
            self.assertEqual(report['tools'], {'lean_sync': 1})
            self.assertEqual(report['malformed_lines'], 0)

    def test_rotation_and_truncation_reset_aggregates(self):
        self.path.write_text(self.event('model', turn=5, usage={'input_tokens': 999}))
        self.reader.read()
        self.path.rename(self.path.with_suffix('.old'))
        self.path.write_text(self.event('tool', name='read_file'))
        report = self.reader.read()
        self.assertEqual(report['attempts'][0]['input'], 0)
        self.assertEqual(report['tools'], {'read_file': 1})
        self.path.write_text('')
        self.assertEqual(self.reader.read()['attempts'], [])

    def test_missing_file_can_appear_later(self):
        self.assertIn('error', self.reader.read())
        self.path.write_text(self.event('attempt_start', label='agent-1'))
        self.assertNotIn('error', self.reader.read())
        self.assertEqual(self.reader.read()['attempts'][0]['label'], 'agent-1')

    def test_malformed_lines_do_not_hide_later_events(self):
        self.path.write_text('broken\n[]\n' + self.event('verification_start'))
        self.assertEqual(self.reader.read()['malformed_lines'], 2)
        with self.path.open('a') as stream:
            stream.write(self.event('attempt_end', status='verified'))
        report = self.reader.read()
        self.assertEqual(report['malformed_lines'], 2)
        self.assertEqual(report['attempts'][0]['status'], 'verified')
        self.assertIsNone(report['attempts'][0]['active'])

    def test_age_refreshes_without_new_events(self):
        self.path.write_text(self.event('model_start'))
        with patch.object(reporter.time, 'time', return_value=2):
            first = self.reader.read()
        with patch.object(reporter.time, 'time', return_value=7):
            second = self.reader.read()
        self.assertEqual(first['attempts'][0]['seconds_since_event'], 1)
        self.assertEqual(second['attempts'][0]['seconds_since_event'], 6)
        self.assertEqual(second['attempts'][0]['status'], 'unfinished')

    def test_compaction_tokens_are_counted_without_incrementing_proof_turns(self):
        self.path.write_text(self.event('model', turn=100, usage={'input_tokens': 50}) +
                             self.event('compaction_start'))
        self.assertEqual(self.reader.read()['attempts'][0]['active'], 'compaction')
        with self.path.open('a') as stream:
            stream.write(self.event('compaction_model', usage={
                'input_tokens': 20, 'cached_input_tokens': 10, 'output_tokens': 5}))
            stream.write(self.event('compaction'))
        row, = self.reader.read()['attempts']
        self.assertEqual((row['turns'], row['input'], row['cached'], row['output']),
                         (100, 70, 10, 5))
        self.assertEqual(row['compactions'], 1)

    def test_json_works_without_site_packages_and_never_has_color(self):
        self.path.write_text(self.event('attempt_start'))
        result = subprocess.run([sys.executable, '-S', str(SCRIPT), str(self.path),
                                 '--json', '--color', 'always'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('\033', result.stdout)
        self.assertEqual(json.loads(result.stdout)[0]['attempts'][0]['id'], 'attempt-000001')


if __name__ == '__main__':
    unittest.main()
