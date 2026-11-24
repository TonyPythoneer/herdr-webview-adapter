import unittest
from unittest.mock import patch
from pathlib import Path
import tempfile

import performance_profile as p
import performance_feedback as f
from test_performance_feedback import incremental_fixture, norm, RUN, REPO

class ProbeTests(unittest.TestCase):
    def test_regression_priority_then_cost_and_max_two(self):
        rounds = [{"candidate": {"samples": {
            "chrome_session_open_ms": [100, 100],
            "chrome_first_frame_receive_ms": [200, 200],
            "chrome_screenshot_png_ms": [100, 100],
            "chrome_eval_1mib_ms": [1, 1],
        }}}] * 5
        self.assertEqual(p.select_probes(rounds, []), ["session_first_frame", "screenshot"])
        self.assertEqual(p.select_probes(rounds, [{"metric": "chrome_eval_1mib_ms",
                         "classification": "regression signal"}]), ["eval_1mib", "session_first_frame"])
        self.assertEqual(p.select_probes([], []), [])

    def test_collapsed_stack_counts(self):
        text = """Call graph:
    500 arbitrary caller
Sort by top of stack, same collapsed (when >= 5):
        __psynch_cvwait  (in libsystem_kernel.dylib)        300
        read  (in libsystem_kernel.dylib)        100
        serde_json::read::foo  (in perf_fixed)        20
        base64::engine::decode  (in perf_fixed)        30
        malloc  (in libsystem_malloc.dylib)        10
        unknown  (in perf_fixed)        5
Binary Images:
        irrelevant 123
"""
        counts = p.summarize_sample(text)
        self.assertEqual(counts["wait_or_sleep"], 300)
        # A read symbol inside a JSON path should be treated as a generic
        # read syscall only when it is an actual syscall name.
        self.assertEqual(counts["read_syscall"], 100)
        self.assertEqual(counts["json"], 20)
        self.assertEqual(sum(counts.values()), 465)
        self.assertEqual(counts["base64"], 30)
        self.assertEqual(counts["allocation"], 10)
        self.assertEqual(counts["other"], 5)
        with self.assertRaises(ValueError):
            p.summarize_sample("no collapsed table")
        with self.assertRaises(ValueError):
            p.summarize_sample("Sort by top of stack, same collapsed (when >= 5):\nBinary Images:")

    def test_sampler_missing_is_explicit(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(Path, "is_file", return_value=False):
            path = Path(directory)
            result = p.run_probe(path/"bin", "screenshot", path/"work", "http://127.0.0.1/", path)
            self.assertEqual(result, {"operation": "screenshot", "status": "unavailable",
                                      "reason": "sampler_unavailable"})

    def test_valid_and_untrusted_optional_feedback(self):
        data = incremental_fixture("native")
        measured = {"operation": "screenshot", "status": "measured", "operations": 20,
                    "elapsed_ms": 6200, "sample_seconds": 2, "interval_ms": 10,
                    "top_stack_counts": dict.fromkeys(p.CATEGORIES, 2)}
        data["profiling"] = {"schema": 1, "method": "macos_sample_adapter_top_of_stack",
                            "scope": "candidate_only_diagnostic",
                            "probes": [measured, {"operation": "session_first_frame",
                                                   "status": "unavailable", "reason": "sampler_failed"}]}
        native = norm(data, "native")
        self.assertEqual(native["profiling"]["status"], "partial")
        self.assertIn("chrome_screenshot_png_ms", native["profiling"]["probes"][0]["related_metrics"])
        report = f.assemble(RUN, {"native": native, "portable": norm(incremental_fixture("portable"), "portable")}, [])
        self.assertEqual(report["status"], "complete")
        self.assertIn("Targeted diagnostic profiling", f.render_comment(report, REPO, 99))
        measured["top_stack_counts"]["@victim"] = 10
        invalid = norm(data, "native")
        self.assertEqual(invalid["status"], "measured")
        self.assertEqual(invalid["profiling"]["status"], "invalid_diagnostic")
        self.assertNotIn("@victim", str(invalid["profiling"]))

    def test_legacy_and_portable(self):
        self.assertEqual(f.normalize_profile({}, "native")["status"], "unavailable")
        self.assertEqual(f.normalize_profile({}, "portable")["status"], "not_applicable")
