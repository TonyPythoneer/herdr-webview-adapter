import io
import copy
import json
import tempfile
from pathlib import Path
import os
import sys
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
    def test_legacy_pr_requires_direct_evidence(self):
        suites = {s: norm(fixture(s), s) for s in f.SUITES}
        report = f.assemble(RUN, suites, [], 7)
        self.assertEqual(report["status"], "complete")
        self.assertEqual(report["pr_base_shas"], [BASE])
        self.assertGreater(report["signals"]["regression_signal"], 0)
        self.assertEqual(report["decision"]["primary_action"]["priority"], "P0")
        self.assertEqual(report["decision"]["scope"], "unavailable")
        self.assertIsNone(report["decision"]["signals"])
        self.assertEqual(report["decision"]["primary_action"]["reason"], "missing_incremental_comparison")
        self.assertEqual(suites["portable"]["metrics"][0]["baseline"]["n"], 500)
        self.assertAlmostEqual(suites["portable"]["metrics"][0]["paired_delta_pct"], 20)
        self.assertIn("not proof of this PR", f.render_comment(report, REPO, 99))
        self.assertIn("benchmark-feedback.json", f.render_comment(report, REPO, 99))

    def test_hotspots_and_lock_confounder(self):
        data = incremental_fixture("native", 1)
        data["lock_sha256"]["candidate"] = "f" * 64
        suites = {"native": norm(data, "native"), "portable": norm(incremental_fixture("portable", 1), "portable")}
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


def incremental_fixture(matrix, ratio=1.2, base_ratio=1.0, reference=BASE):
    data = fixture(matrix, ratio)
    data["schema"] = 2
    data["incremental_reference_sha"] = reference
    data["context"]["pr_base_sha"] = reference
    data["lock_sha256"]["pr_base"] = data["lock_sha256"]["baseline"]
    data["round_order"] = {}
    for workload, pairs in data["rounds"].items():
        data["round_order"][workload] = []
        for i, pair in enumerate(pairs):
            pair["pr_base"] = copy.deepcopy(pair["baseline"])
            for key, samples in pair["pr_base"]["samples"].items():
                pair["pr_base"]["samples"][key] = [x * base_ratio for x in samples]
            order = (("baseline", "pr_base", "candidate"),
                     ("candidate", "baseline", "pr_base"),
                     ("pr_base", "candidate", "baseline"),
                     ("candidate", "pr_base", "baseline"),
                     ("baseline", "candidate", "pr_base"))[i] if reference != f.BASELINE else (
                         ("baseline", "candidate") if i % 2 == 0 else ("candidate", "baseline"))
            data["round_order"][workload].append(list(order))
    return data

class IncrementalTests(unittest.TestCase):
    def test_cumulative_regression_but_incremental_improvement(self):
        suites = {s: norm(incremental_fixture(s, 1.2, 1.5), s) for s in f.SUITES}
        report = f.assemble(RUN, suites, [], 7)
        self.assertEqual(report["schema"], "herdr.performance-feedback/v2")
        self.assertEqual(report["incremental"]["status"], "complete")
        self.assertEqual(report["decision_scope"], "incremental")
        self.assertGreater(report["signals"]["regression_signal"], 0)
        self.assertGreater(report["incremental"]["signals"]["improvement_signal"], 0)
        self.assertFalse(any(a.get("scope") == "incremental" and a.get("priority") == "P1" for a in report["next_actions"]))
        self.assertEqual(report["decision"]["outcome"], "improvement_signal")
        self.assertEqual(report["decision"]["primary_action"]["reason"], "review_improvement_signal")
        row = suites["portable"]["incremental_metrics"][0]
        self.assertAlmostEqual(row["paired_delta_pct"], -20)
        self.assertEqual(row["raw_samples_by_pair"]["candidate"], suites["portable"]["metrics"][0]["raw_samples_by_pair"]["candidate"])
        self.assertIn("Incremental comparison", f.render_comment(report, REPO, 99))

    def test_incremental_regression_despite_cumulative_improvement(self):
        suites = {s: norm(incremental_fixture(s, .8, .6), s) for s in f.SUITES}
        suites["native"]["lock_sha256"]["candidate"] = "f" * 64
        report = f.assemble(RUN, suites, [])
        self.assertEqual(report["decision"]["outcome"], "regression_signal")
        self.assertEqual(report["decision"]["lock_drift_suites"], ["native"])
        summary = f.render_comment(report, REPO, 99).split("<details>")[0]
        self.assertIn("locks differ in native", summary)
        self.assertIn(report["decision"]["primary_action"]["metric"], summary)
        self.assertIn("Paired median", summary)
        self.assertGreater(report["signals"]["improvement_signal"], 0)
        self.assertGreater(report["incremental"]["signals"]["regression_signal"], 0)
        self.assertEqual(report["next_actions"][0]["scope"], "incremental")

    def test_legacy_and_partial_not_incremental_complete(self):
        self.assertEqual(norm(fixture("portable"), "portable")["incremental_status"], "unavailable")
        data = incremental_fixture("portable")
        data["status"] = "failed"
        data["rounds"]["portable"].pop()
        data["round_order"]["portable"].pop()
        report = f.assemble(dict(RUN, conclusion="failure"), {"portable": norm(data, "portable")}, [])
        self.assertEqual(report["incremental"]["status"], "unavailable_or_partial")
        self.assertEqual(report["next_actions"][0]["priority"], "P0")

    def test_reject_reference_order_and_missing_base(self):
        for mutate in [
            lambda d: d.update(incremental_reference_sha=HEAD),
            lambda d: d["round_order"]["portable"][0].reverse(),
            lambda d: d["rounds"]["portable"][0].pop("pr_base"),
            lambda d: d["lock_sha256"].pop("pr_base"),
        ]:
            data = incremental_fixture("portable")
            mutate(data)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                norm(data, "portable")

    def test_same_reference_is_reused_and_must_match(self):
        data = incremental_fixture("portable", reference=f.BASELINE)
        result = norm(data, "portable")
        self.assertEqual(result["metrics"], result["incremental_metrics"])
        data["rounds"]["portable"][0]["pr_base"]["samples"]["url_policy_5000_calls_ms"][0] = 200
        with self.assertRaises(ValueError):
            norm(data, "portable")

    def test_push_not_applicable(self):
        data = fixture("portable")
        data.update(schema=2, incremental_reference_sha=None, candidate_sha=HEAD)
        data["context"].update(event="push", pr_base_sha=None, pr_head_sha=None)
        data["round_order"] = {"portable": [["baseline", "candidate"] if i % 2 == 0 else ["candidate", "baseline"] for i in range(f.PAIRS)]}
        result = f.normalize_suite(data, "portable", 12, 2, "push", HEAD)
        self.assertEqual(result["incremental_status"], "not_applicable")

    def test_cumulative_only_summary(self):
        for event in ("push", "workflow_dispatch"):
            suites = {}
            for name in f.SUITES:
                data = fixture(name, 1)
                data["context"]["event"] = event
                data["candidate_sha"] = HEAD
                suites[name] = f.normalize_suite(data, name, 12, 2, event, HEAD)
            report = f.assemble(dict(RUN, event=event), suites, [])
            self.assertEqual(report["decision"]["scope"], "cumulative")
            self.assertEqual(report["decision"]["outcome"], "inconclusive")
            self.assertIn("Fixed baseline vs candidate", f.render_comment(report, REPO, 99).split("<details>")[0])

    def test_hotspot_links_from_first_screen_to_stack(self):
        suites = {s: norm(incremental_fixture(s, 1), s) for s in f.SUITES}
        # Make screenshots the largest accumulated cost without changing a paired delta.
        for row in suites["native"]["metrics"]:
            if row["metric"] == "chrome_screenshot_png_ms":
                row["candidate_median_sum_per_pair"] = 9999
        suites["native"]["artifact_id"] = 456
        suites["native"]["profiling"] = {"status": "measured", "probes": [
            {"operation": "screenshot", "status": "measured", "source_log": "profile-screenshot.sample.txt",
             "operations": 42, "top_stack_counts": {"wait_or_sleep": 10}}]}
        report = f.assemble(RUN, suites, [])
        decision = report["decision"]
        self.assertEqual(decision["scope"], "incremental")
        self.assertEqual(decision["signals"]["inconclusive"], 23)
        self.assertEqual(decision["primary_action"], report["next_actions"][0])
        self.assertEqual(decision["primary_action"]["suite"], "native")
        summary = f.render_comment(report, REPO, 99).split("<details>")[0]
        for expected in ("23 inconclusive", "9,999.0 ms", "profile-screenshot.sample.txt", "/runs/12/artifacts/456"):
            self.assertIn(expected, summary)
        self.assertNotIn("| Metric |", summary)

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


    def test_publish_replay_and_partial_artifact(self):
        run = dict(RUN, workflow_id=5, path=".github/workflows/performance.yml",
                   repository={"full_name": REPO}, head_repository={"full_name": REPO}, status="completed")
        pr = {"number": 7, "state": "open", "head": {"sha": HEAD}, "base": {"sha": BASE}}
        comments = []
        archives = {}
        for i, suite in enumerate(f.SUITES, 1):
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, "w") as z:
                z.writestr("performance.json", json.dumps(fixture(suite)))
            archives[i] = buf.getvalue()
        def fake_api(path, method="GET", payload=None, binary=False):
            if path.endswith("workflows/performance.yml"):
                return {"id": 5}
            if path.endswith("/runs/12"):
                return run
            if "/runs/12/artifacts?" in path:
                return {"artifacts": [{"id": i, "name": f"adapter-performance-{suite}-2",
                                       "size_in_bytes": len(archives[i]), "expired": False}
                                      for i, suite in enumerate(f.SUITES, 1) if i in archives]}
            if path.endswith("/zip"):
                return archives[int(path.split("/")[-2])]
            if "/commits/" in path:
                return {"parents": [{"sha": BASE}, {"sha": HEAD}]}
            if path.endswith("/pulls/7"):
                return pr
            if method == "POST":
                comments.append({"id": 42, "user": {"id": pub.BOT_ID}, "body": payload["body"]})
                return comments[-1]
            if method == "PATCH":
                comments[0]["body"] = payload["body"]
                return comments[0]
            raise AssertionError(path)
        with tempfile.TemporaryDirectory() as tmp, patch.object(pub, "api", side_effect=fake_api), \
             patch.object(pub, "match_pr", return_value=pr), patch.object(pub, "newest_run", return_value=True), \
             patch.object(pub, "paged", side_effect=lambda *a, **kw: iter(comments)), \
             patch.dict(os.environ, {"GITHUB_REPOSITORY": REPO, "GITHUB_RUN_ID": "99", "GITHUB_STEP_SUMMARY": ""}), \
             patch.object(sys, "argv", ["report", "--run-id", "12", "--out", tmp]):
            self.assertEqual(pub.main(), 0)
            self.assertEqual(len(comments), 1)
            self.assertEqual(json.loads((Path(tmp) / "benchmark-feedback.json").read_text())["status"], "complete")
            self.assertEqual(pub.main(), 0)
            self.assertEqual(len(comments), 1)
            archives.pop(2)
            self.assertEqual(pub.main(), 0)
            self.assertEqual(len(comments), 1)
            self.assertIn("partial or failed", comments[0]["body"])
            report = json.loads((Path(tmp) / "benchmark-feedback.json").read_text())
            self.assertEqual(report["next_actions"][0]["priority"], "P0")
            pr["head"]["sha"] = BASE
            previous = comments[0]["body"]
            self.assertEqual(pub.main(), 0)
            self.assertEqual(comments[0]["body"], previous)

if __name__ == "__main__":
    unittest.main()
