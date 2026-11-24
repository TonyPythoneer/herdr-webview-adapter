import unittest
from runtime_cache_experiment import comparison, ARMS
import itertools

class ComparisonTests(unittest.TestCase):
    def test_all_orderings(self):
        self.assertEqual(len(set(itertools.permutations(ARMS))), 6)

    def test_consistent_gain(self):
        rounds = [{"a":{"segments_ms":70}, "b":{"segments_ms":100}} for _ in range(6)]
        result = comparison(rounds, "a", "b")
        self.assertEqual(result["screening_signal"], "improvement")
        self.assertAlmostEqual(result["median_pct"], -30)

    def test_noise_not_gain(self):
        rounds = [{"a":{"segments_ms":n}, "b":{"segments_ms":100}} for n in [70,70,70,70,70,99]]
        self.assertEqual(comparison(rounds, "a", "b")["screening_signal"], "inconclusive")

    def test_regression(self):
        rounds = [{"a":{"segments_ms":120}, "b":{"segments_ms":100}} for _ in range(6)]
        self.assertEqual(comparison(rounds, "a", "b")["screening_signal"], "regression")
