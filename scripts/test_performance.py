import unittest
import os
from pathlib import Path
import tempfile
from unittest.mock import patch
import performance as p

class ComparisonTests(unittest.TestCase):
    def test_quantiles(self):
        self.assertEqual(p.summarize(list(range(1,101))), {"n":100,"p50":50,"p95":95,"p99":99})
    def pair(self, base, candidate):
        return {"baseline":{"samples":{"metric":[base]},"meta":{}},"candidate":{"samples":{"metric":[candidate]},"meta":{}}}
    def test_improvement(self):
        self.assertEqual(p.compare([self.pair(100,80)]*p.PAIRS,"test")[0]["classification"],"improvement signal")
    def test_regression(self):
        self.assertEqual(p.compare([self.pair(100,120)]*p.PAIRS,"test")[0]["classification"],"regression signal")
    def test_inconclusive(self):
        self.assertEqual(p.compare([self.pair(100,80)]*4+[self.pair(100,101)],"test")[0]["classification"],"inconclusive")
    def test_incomplete(self):
        with self.assertRaises(RuntimeError):
            p.compare([self.pair(100,80)],"test")
    def test_different_browser(self):
        pair=self.pair(100,80)
        pair["candidate"]["meta"]={"chrome_version":"different"}
        with self.assertRaises(RuntimeError):
            p.compare([pair]*p.PAIRS,"test")

class IncrementalHarnessTests(unittest.TestCase):
    def test_round_order_and_candidate_reuse(self):
        orders = [p.round_order(i, True) for i in range(p.PAIRS)]
        self.assertEqual(len(set(orders)), p.PAIRS)
        for order in orders:
            self.assertEqual(set(order), {"baseline", "pr_base", "candidate"})
        for a, b in (("baseline", "candidate"), ("pr_base", "candidate"), ("baseline", "pr_base")):
            self.assertIn(sum(o.index(a) < o.index(b) for o in orders), (2, 3))
        candidate = {"samples": {"metric": [80]}, "meta": {}}
        rounds = [{"baseline": {"samples": {"metric": [100]}, "meta": {}},
                   "pr_base": {"samples": {"metric": [60]}, "meta": {}}, "candidate": candidate}] * p.PAIRS
        fixed = p.comparison_pairs(rounds, "baseline")
        incremental = p.comparison_pairs(rounds, "pr_base")
        self.assertIs(fixed[0]["candidate"], incremental[0]["candidate"])
        self.assertEqual(p.compare(fixed, "test")[0]["classification"], "improvement signal")
        self.assertEqual(p.compare(incremental, "test")[0]["classification"], "regression signal")

    def test_resolve_exact_merge_parents(self):
        context = {"event": "pull_request", "pr_base_sha": "a" * 40, "pr_head_sha": "b" * 40}
        with patch.object(p, "run", return_value="a" * 40 + " " + "b" * 40):
            self.assertEqual(p.resolve_pr_base("c" * 40, context), "a" * 40)
        with patch.object(p, "run", return_value="d" * 40 + " " + "b" * 40):
            with self.assertRaises(RuntimeError):
                p.resolve_pr_base("c" * 40, context)
        with self.assertRaises(RuntimeError):
            p.resolve_pr_base("c" * 40, dict(context, pr_base_sha="main"))
        self.assertIsNone(p.resolve_pr_base("c" * 40, {"event": "push"}))

    def test_lock_reuse_and_preserve_committed_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, prior, candidate = [Path(tmp) / name for name in ("base", "prior", "candidate")]
            for tree in (base, prior, candidate):
                tree.mkdir()
                (tree / "Cargo.toml").write_text("same")
            (base / "Cargo.lock").write_text("baseline lock")
            (prior / "Cargo.lock").write_text("PR base lock")
            with patch.object(p.subprocess, "run") as run:
                self.assertEqual(p.prepare_lock(candidate, [prior, base]).read_text(), "PR base lock")
                (candidate / "Cargo.lock").write_text("committed candidate lock")
                self.assertEqual(p.prepare_lock(candidate, [prior, base]).read_text(), "committed candidate lock")
                run.assert_not_called()
            (candidate / "Cargo.lock").unlink()
            (candidate / "Cargo.toml").write_text("changed manifest")
            def generate(*args, **kwargs):
                (candidate / "Cargo.lock").write_text("new lock")
            with patch.object(p.subprocess, "run", side_effect=generate) as run:
                self.assertEqual(p.prepare_lock(candidate, [prior, base]).read_text(), "new lock")
                run.assert_called_once()

class BuildReuseTests(unittest.TestCase):
    def test_shared_target_rebuild_and_revision_snapshots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            baseline, candidate = root / "baseline", root / "candidate"
            baseline.mkdir()
            candidate.mkdir()
            calls = []
            def cargo(args, **kwargs):
                calls.append((args, kwargs["cwd"]))
                if args[1] == "build":
                    output = target / "release" / "examples" / "perf_fixed"
                    output.parent.mkdir(parents=True, exist_ok=True)
                    output.write_bytes(kwargs["cwd"].name.encode())
                    output.chmod(0o755)
            with patch.object(p.subprocess, "run", side_effect=cargo):
                first = p.build(baseline, False, target, root / "binaries" / "baseline")
                second = p.build(candidate, False, target, root / "binaries" / "candidate")
            self.assertEqual(first["perf_fixed"].read_bytes(), b"baseline")
            self.assertEqual(second["perf_fixed"].read_bytes(), b"candidate")
            self.assertNotEqual(p.digest(first["perf_fixed"]), p.digest(second["perf_fixed"]))
            self.assertEqual([args[1] for args, _ in calls], ["build", "clean", "build"])
            self.assertIn("herdr-webview-adapter", calls[1][0])
            self.assertTrue(os.access(second["perf_fixed"], os.X_OK))

class NativeProofTests(unittest.TestCase):
    def test_pixel_missing_final_proof_fails(self):
        with self.assertRaisesRegex(RuntimeError, "WK_PIXEL_VERIFIED"):
            p.require_native_proof("wk_native_pixels", "native WKWebView screenshot pixel test passed")
    def test_pixel_complete_proof(self):
        p.require_native_proof("wk_native_pixels", "native WKWebView screenshot pixel test passed\nWK_PIXEL_VERIFIED:")
    def test_input_missing_proof_fails(self):
        with self.assertRaisesRegex(RuntimeError, "WK_TRUSTED_WHEEL_VERIFIED"):
            p.require_native_proof("wk_native_input", "WK_TRUSTED_INPUT_VERIFIED:")
    def test_input_complete_proof(self):
        p.require_native_proof("wk_native_input", "WK_TRUSTED_KEY_VERIFIED:\nWK_TRUSTED_TEXT_VERIFIED:\nWK_TRUSTED_WHEEL_VERIFIED:\nWK_TRUSTED_INPUT_VERIFIED:")


class UserLatencyTests(unittest.TestCase):
    def test_cold_and_warm_are_matched_within_process(self):
        raw = {"chrome_client_start_ms": [100], "chrome_session_open_ms": [20, 30, 40],
               "chrome_first_frame_receive_ms": [5, 6, 7]}
        self.assertEqual(p.user_latency_samples(raw), {"cold_start_open_frame_segments_ms": [125],
                                                       "warm_open_frame_segments_ms": [36, 47]})
        with self.assertRaises(RuntimeError):
            p.user_latency_samples(dict(raw, chrome_session_open_ms=[20]))
