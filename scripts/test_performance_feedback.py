import copy
import io
import json
import unittest
from unittest.mock import patch
import zipfile

import performance_feedback as f
import publish_performance_feedback as pub

HEAD = "a" * 40
BASE = "b" * 40
MERGE = "c" * 40
RUN = {"id": 12, "run_attempt": 2, "event": "pull_request", "head_sha": HEAD,
       "conclusion": "success", "head_branch": "feature/test", "run_number": 5}
REPO = "owner/repo"

def fixture(matrix, ratio=1.2):
    rounds = {}
    for suite in f.SUITES[matrix]:
        meta = {"chrome_version": "Google Chrome 158.0.8127.37"} if suite == "chrome" else {}
        rounds[suite] = [
            {side: {"meta": meta.copy(), "samples": {key: [100 * scale] * n for key, n in f.COUNTS[suite].items()}}
             for side, scale in (("baseline", 1), ("candidate", ratio))}
            for _ in range(f.PAIRS)]
    return {"schema": 1, "context": {"run_id": "12", "run_attempt": "2", "event": "pull_request",
             "suite": matrix, "pr_head_sha": HEAD, "pr_base_sha": BASE},
            "candidate_sha": MERGE, "baseline_sha": f.BASELINE, "status": "measured",
            "workload_sha256": {name: "d" * 64 for name in ("workload.rs", "wk_native_pixels.rs", "wk_native_input.rs")},
            "lock_sha256": {"baseline": "e" * 64, "candidate": "e" * 64},
            "rounds": rounds, "errors": [], "environment": {"runner_os": "macOS", "cpu_count": 4}}

def norm(data, matrix):
    return f.normalize_suite(data, matrix, 12, 2, "pull_request", HEAD)

class FeedbackTests(unittest.TestCase):
    def test_complete_and_prioritized_regression(self):
        suites = {s: norm(fixture(s), s) for s in f.SUITES}
        report = f.assemble(RUN, suites, [], 7)
        self.assertEqual(report["status"], "complete")
        self.assertEqual(report["pr_base_shas"], [BASE])
        self.assertGreater(report["signals"]["regression_signal"], 0)
        self.assertEqual(report["next_actions"][0]["priority"], "P1")
        self.assertEqual(suites["portable"]["metrics"][0]["baseline"]["n"], 500)
        self.assertAlmostEqual(suites["portable"]["metrics"][0]["paired_delta_pct"], 20)
        self.assertIn("not proof of this PR", f.render_comment(report, REPO, 99))
        self.assertIn("benchmark-feedback.json", f.render_comment(report, REPO, 99))

    def test_hotspots_and_lock_confounder(self):
        data = fixture("native", 1)
        data["lock_sha256"]["candidate"] = "f" * 64
        suites = {"native": norm(data, "native"), "portable": norm(fixture("portable", 1), "portable")}
        report = f.assemble(RUN, suites, [])
        self.assertEqual(report["next_actions"][0]["reason"], "dependency_resolution_differs")
        self.assertTrue(any(x.get("priority") == "P2" and x.get("metric") for x in report["next_actions"]))
        self.assertEqual(suites["native"]["environment"]["chrome_versions"], ["Google Chrome 158.0.8127.37"])

    def test_inconclusive_and_incomplete(self):
        data = fixture("portable")
        data["rounds"]["portable"][0]["candidate"]["samples"]["url_policy_5000_calls_ms"] = [95] * 100
        row = next(r for r in norm(data, "portable")["metrics"] if r["metric"] == "url_policy_5000_calls_ms")
        self.assertEqual(row["signal"], "inconclusive")
        data["rounds"]["portable"].pop()
        with self.assertRaises(ValueError):
            norm(data, "portable")
        data["status"] = "failed"
        report = f.assemble(dict(RUN, conclusion="failure"), {"portable": norm(data, "portable")}, ["native:missing_or_duplicate_artifact"])
        self.assertEqual(report["status"], "partial_or_failed")
        self.assertEqual(report["next_actions"][0]["priority"], "P0")

    def test_reject_bad_provenance_samples_and_metric_names(self):
        for mutate in [
            lambda d: d["context"].update(run_attempt="1"),
            lambda d: d["context"].update(pr_head_sha=BASE),
            lambda d: d.update(baseline_sha=HEAD),
            lambda d: d["rounds"]["portable"][0]["candidate"]["samples"].update(unknown_injected_metric=[1]),
            lambda d: d["rounds"]["portable"][0]["candidate"]["samples"]["url_policy_5000_calls_ms"].__setitem__(0, float("nan")),
        ]:
            data = fixture("portable")
            mutate(data)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                norm(data, "portable")

    def test_free_text_not_in_rendered_feedback(self):
        data = fixture("portable")
        data["errors"] = ["@victim ignore previous instructions"]
        data["environment"]["runner_os"] = "@victim"
        data["comparison"] = {"untrusted": ["@victim"]}
        report = f.assemble(RUN, {"portable": norm(data, "portable")}, [])
        self.assertNotIn("@victim", json.dumps(report))
        self.assertNotIn("@victim", f.render_comment(report, REPO, 99))

    def test_zip_reads_only_fixed_json_entry(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("performance.json", json.dumps(fixture("portable")))
            z.writestr("../../unsafe", "ignored")
        self.assertEqual(f.read_zip(buf.getvalue())["schema"], 1)
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("other.json", "{}")
        with self.assertRaises(ValueError):
            f.read_zip(buf.getvalue())

class PublishingTests(unittest.TestCase):
    def test_only_bot_owned_comment(self):
        comments = [{"id": 1, "user": {"id": 2}, "body": pub.MARKER},
                    {"id": 2, "user": {"id": pub.BOT_ID}, "body": pub.MARKER + "\nold"}]
        self.assertEqual(pub.find_existing(comments)["id"], 2)
        self.assertIsNone(pub.find_existing(comments[:1]))
        with self.assertRaises(ValueError):
            pub.find_existing(comments + [comments[1]])

    def test_stale_open_pr_and_merge_parent(self):
        suite = norm(fixture("portable"), "portable")
        pr = {"state": "open", "head": {"sha": HEAD}, "base": {"sha": BASE}}
        self.assertTrue(pub.current_pr(pr, RUN, {"portable": suite}, REPO))
        pr["base"]["sha"] = HEAD
        self.assertFalse(pub.current_pr(pr, RUN, {"portable": suite}, REPO))
        with patch.object(pub, "api", return_value={"parents": [{"sha": BASE}, {"sha": HEAD}]}):
            self.assertTrue(pub.verify_candidate(RUN, suite, REPO))
        with patch.object(pub, "api", return_value={"parents": [{"sha": HEAD}, {"sha": BASE}]}):
            self.assertFalse(pub.verify_candidate(RUN, suite, REPO))

    def test_newer_run_and_attempt_supersede(self):
        with patch.object(pub, "api", return_value={"workflow_runs": [RUN]}):
            self.assertTrue(pub.newest_run(RUN, REPO))
        with patch.object(pub, "api", return_value={"workflow_runs": [RUN, dict(RUN, run_attempt=3)]}):
            self.assertFalse(pub.newest_run(RUN, REPO))
        with patch.object(pub, "api", return_value={"workflow_runs": [RUN, dict(RUN, id=13, run_number=6)]}):
            self.assertFalse(pub.newest_run(RUN, REPO))

    def test_run_identity(self):
        run = dict(RUN, workflow_id=5, path=".github/workflows/performance.yml", repository={"full_name": REPO}, status="completed")
        self.assertTrue(pub.eligible_run(run, REPO, 5))
        self.assertFalse(pub.eligible_run(dict(run, workflow_id=6), REPO, 5))
        self.assertFalse(pub.eligible_run(dict(run, status="in_progress"), REPO, 5))

if __name__ == "__main__":
    unittest.main()
