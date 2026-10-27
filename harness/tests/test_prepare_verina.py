"""Check proof stripping without model calls or reference proof exposure."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('prepare_verina',
    Path(__file__).resolve().parents[2] / 'scripts/prepare-verina.py')
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class ProofInput(unittest.TestCase):
    def test_only_proof_regions_are_removed(self):
        prefix = ('-- !benchmark @start code\ndef f := 42\n-- !benchmark @end code\n'
                  '-- !benchmark @start proof_aux\nlemma reference_hint : True := by trivial\n'
                  '-- !benchmark @end proof_aux\n'
                  'theorem f_spec_satisfied : f = 42 := by\n')
        source = prefix + ('  -- !benchmark @start proof\n  exact reference_answer\n'
                           '  -- !benchmark @end proof\n')
        result = prepare.proof_input(source)
        self.assertIn('def f := 42', result)
        self.assertIn('theorem f_spec_satisfied : f = 42 := by', result)
        self.assertNotIn('reference_hint', result)
        self.assertNotIn('reference_answer', result)
        self.assertIn('  sorry\n', result)

    def test_other_admissions_and_bad_markers_are_rejected(self):
        valid = ('-- !benchmark @start proof_aux\n-- !benchmark @end proof_aux\n'
                 '-- !benchmark @start proof\nsorry\n-- !benchmark @end proof\n')
        for source in [valid + 'def hidden : Nat := sorry\n',
                       valid.replace('@end proof_aux', '@end code'),
                       valid.replace('-- !benchmark @end proof\n', ''), valid + valid]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                prepare.proof_input(source)

    def test_library_ports_are_recorded_without_changing_goals(self):
        source = ('def a := Array.mkArray 2 0\n'
                  'theorem target : a.size = 2 := by sorry\n')
        result, edits = prepare.compatible_input(source, 'verina_basic_64')
        self.assertEqual(result, source.replace('Array.mkArray', 'Array.replicate'))
        self.assertEqual(edits, [{'old': 'Array.mkArray', 'new': 'Array.replicate', 'count': 1}])
        self.assertEqual(prepare.compatible_input('theorem target : True := by sorry\n',
                         'verina_basic_1'), ('theorem target : True := by sorry\n', []))
        current = 'def s : Std.HashSet Nat := Std.HashSet.emptyWithCapacity\n'
        self.assertEqual(prepare.compatible_input(current, 'verina_basic_22'), (current, []))


if __name__ == '__main__':
    unittest.main()
