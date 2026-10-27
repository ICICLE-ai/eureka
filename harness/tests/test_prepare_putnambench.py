"""Keep fixed answers and theorem statements intact when importing PutnamBench."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('prepare_putnambench',
    Path(__file__).resolve().parents[2] / 'scripts/prepare-putnambench.py')
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class ProofInput(unittest.TestCase):
    def test_official_answer_is_fixed_and_theorem_is_unchanged(self):
        theorem = 'theorem putnam_test : 1 + 1 = putnam_test_solution := sorry\n'
        source = ('import Mathlib\nabbrev putnam_test_solution : ℕ := sorry\n'
                  '--2\n' + theorem)
        result, answers = prepare.proof_input(source, 'putnam_test')
        self.assertTrue(result.endswith(theorem))
        self.assertIn('noncomputable abbrev putnam_test_solution : ℕ := 2\n', result)
        self.assertEqual(answers, [{'name': 'putnam_test_solution', 'value': '2'}])

    def test_problem_without_answer_hole_is_byte_identical(self):
        source = 'import Mathlib\ntheorem putnam_test : True := sorry\n'
        self.assertEqual(prepare.proof_input(source, 'putnam_test'), (source, []))

    def test_missing_answers_other_holes_and_axioms_stop_import(self):
        theorem = 'theorem putnam_test : True := sorry\n'
        sources = ['abbrev answer : ℕ := sorry\n' + theorem,
                   'abbrev answer : ℕ := sorry\n-- sorry\n' + theorem,
                   'def hidden : ℕ := sorry\n' + theorem,
                   'axiom hidden : False\n' + theorem,
                   theorem.replace('putnam_test', 'wrong_target')]
        for source in sources:
            with self.subTest(source=source), self.assertRaises(ValueError):
                prepare.proof_input(source, 'putnam_test')


if __name__ == '__main__':
    unittest.main()
