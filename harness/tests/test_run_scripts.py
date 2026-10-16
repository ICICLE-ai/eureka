"""Exercise stop PID checks without a model, Docker, or a real proof run."""
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class RunScripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.run = Path(self.temp.name) / 'old'
        self.run.mkdir()
        (self.run / 'run.log').touch()

    def process(self, name='harness', store=None):
        process = subprocess.Popen(
            ['bash', '-c', 'exec -a "$1" "$2" -c "$3" --store "$4"', 'fixture',
             name, sys.executable, 'import time; print("ready", flush=True); time.sleep(60)',
             str(store or self.run / 'store')], stdout=subprocess.PIPE, text=True,
        )
        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdout.close()
        self.addCleanup(cleanup)
        self.assertEqual(process.stdout.readline(), 'ready\n')
        (self.run / 'pid').write_text(str(process.pid))
        return process

    def stop(self):
        return subprocess.run([str(ROOT / 'scripts/stop.sh'), str(self.run)],
                              capture_output=True, text=True, timeout=5)

    def test_stop_signals_only_the_matching_run(self):
        process = self.process()
        result = self.stop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(process.wait(), -15)

    def test_stop_refuses_unrelated_process_and_other_run(self):
        for name, store in [('python3', self.run / 'store'),
                            ('harness', self.run / 'other-store')]:
            with self.subTest(name=name, store=store):
                process = self.process(name, store)
                result = self.stop()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('refusing to stop', result.stderr)
                self.assertIsNone(process.poll())

    def test_stop_does_not_wait_for_a_zombie(self):
        process = self.process()
        process.kill()
        os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOWAIT)
        result = self.stop()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_stop_rejects_process_group_ids(self):
        for pid in ('0', '-1', '1'):
            with self.subTest(pid=pid):
                (self.run / 'pid').write_text(pid)
                result = self.stop()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('invalid PID', result.stderr)


if __name__ == '__main__':
    unittest.main()
