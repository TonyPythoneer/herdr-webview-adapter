#!/usr/bin/env python3
"""Normalize benchmark evidence for agents. Artifact content is untrusted data."""
import json
import math
import re
import statistics
import zipfile
import io

BASELINE = "acf81de1738bf6ef2e4f6194208eb268e7523842"
PAIRS = 5
THRESHOLD = 15
MAX_JSON = 8 * 1024 * 1024
MAX_ZIP = 32 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
SUITES = {"portable": ("portable",), "native": ("chrome", "wk_native_pixels", "wk_native_input")}
COUNTS = {
    "portable": {"url_policy_5000_calls_ms": 100, "engine_policy_2000_calls_ms": 100},
    "chrome": {name + "_ms": n for name, n in {
        "chrome_client_start": 1, "chrome_session_open": 3, "chrome_first_frame_receive": 3,
        "chrome_query": 60, "chrome_eval_roundtrip": 60, "chrome_eval_64kib": 30,
        "chrome_eval_1mib": 30, "chrome_click_to_dom_observed": 15, "chrome_screenshot_png": 15,
        "chrome_session_close_confirmed": 3, "chrome_client_shutdown": 1}.items()},
    "wk_native_pixels": {}, "wk_native_input": {},
}
for suite in SUITES["native"]:
    COUNTS[suite].update({suite + "_workload_wall_ms": 1,
                          suite + "_harness_and_waited_children_cpu_seconds": 1,
                          suite + "_harness_peak_rss_kib": 1})
COUNTS["chrome"]["chrome_descendants_peak_rss_kib"] = 1

def check(condition, reason):
    if not condition:
        raise ValueError(reason)

def read_zip(blob):
    check(len(blob) <= MAX_ZIP, "artifact archive too large")
    with zipfile.ZipFile(io.BytesIO(blob)) as archive:
        entries = [i for i in archive.infolist() if i.filename == "performance.json"]
        check(len(entries) == 1 and entries[0].file_size <= MAX_JSON, "missing, duplicate or oversized report")
        with archive.open(entries[0]) as source:
            data = source.read(MAX_JSON + 1)
        check(len(data) <= MAX_JSON, "report too large")
    return json.loads(data)

def sha(value):
    return isinstance(value, str) and SHA.fullmatch(value) is not None

def digest(value):
    return isinstance(value, str) and DIGEST.fullmatch(value) is not None

def finite_number(value):
    return type(value) in (int, float) and math.isfinite(value) and 0 <= value <= 1e12

def stats(values):
    values = sorted(values)
    def p(q):
        return values[max(0, math.ceil(q * len(values)) - 1)]
    return {"n": len(values), "p50": p(.5), "p95": p(.95), "p99": p(.99)}

def metric_owner(key):
    if key.startswith(("url_policy", "engine_policy")):
        return ["src/browser.rs", "src/browser_backend.rs"]
    if key.startswith(("chrome_eval", "chrome_query")):
        return ["src/chrome_cdp/framing.rs", "src/chrome_cdp.rs", "src/browser_api.rs"]
    if key.startswith(("chrome_first_frame", "chrome_screenshot")):
        return ["src/chrome_cdp.rs", "src/chrome_view.rs"]
    if key.startswith("chrome"):
        return ["src/chrome_cdp.rs"]
    if key.startswith("wk_native_input"):
        return ["src/wk_input.rs", "src/wk_agent.rs"]
    return ["src/wk_native.rs"]

def normalize_suite(data, matrix_suite, run_id, attempt, event, head):
    check(isinstance(data, dict) and data.get("schema") == 1, "unknown source schema")
    context = data.get("context")
    check(isinstance(context, dict), "missing run provenance")
    check(context.get("run_id") == str(run_id) and context.get("run_attempt") == str(attempt)
          and context.get("event") == event and context.get("suite") == matrix_suite, "run context mismatch")
    candidate = data.get("candidate_sha")
    check(sha(candidate) and data.get("baseline_sha") == BASELINE, "invalid revision identity")
    if event == "pull_request":
        check(context.get("pr_head_sha") == head and sha(context.get("pr_base_sha")), "invalid PR identity")
    else:
        check(candidate == head, "candidate differs from workflow head")
    hashes = data.get("workload_sha256")
    check(isinstance(hashes, dict) and set(hashes) == {"workload.rs", "wk_native_pixels.rs", "wk_native_input.rs"}
          and all(digest(v) for v in hashes.values()), "invalid workload hashes")
    locks = data.get("lock_sha256", {})
    check(isinstance(locks, dict) and set(locks) <= {"baseline", "candidate"} and all(digest(v) for v in locks.values()), "invalid lock hashes")
    status = data.get("status")
    check(status in ("measured", "failed"), "invalid source status")
    if status == "measured":
        check(set(locks) == {"baseline", "candidate"}, "missing lock evidence")
    rounds = data.get("rounds")
    check(isinstance(rounds, dict) and set(rounds) <= set(SUITES[matrix_suite]), "unexpected workloads")
    rows = []
    coverage = {}
    browser_versions = set()
    for suite in SUITES[matrix_suite]:
        pairs = rounds.get(suite, [])
        check(isinstance(pairs, list) and len(pairs) <= PAIRS, "invalid pair count")
        coverage[suite] = len(pairs)
        if len(pairs) != PAIRS:
            check(status != "measured", "incomplete measured workload")
            continue
        normalized = {}
        for pair in pairs:
            check(isinstance(pair, dict) and set(pair) == {"baseline", "candidate"}, "invalid pair")
            for side in ("baseline", "candidate"):
                record = pair[side]
                check(isinstance(record, dict) and isinstance(record.get("samples"), dict) and isinstance(record.get("meta"), dict), "invalid observation")
                check(set(record["samples"]) == set(COUNTS[suite]), "metric coverage mismatch")
                for key, n in COUNTS[suite].items():
                    values = record["samples"][key]
                    check(isinstance(values, list) and len(values) == n and all(finite_number(v) for v in values), "invalid samples")
                    normalized.setdefault(key, {"baseline": [], "candidate": []})[side].append(values)
            check(pair["baseline"]["meta"] == pair["candidate"]["meta"], "environment mismatch inside pair")
            if suite == "chrome":
                version = pair["baseline"]["meta"].get("chrome_version")
                check(isinstance(version, str) and re.fullmatch(r"(?:Google Chrome )?[0-9]+(?:\\.[0-9]+){1,4}", version) and len(version) <= 64, "invalid Chrome version")
                browser_versions.add(version)
        for key, samples in sorted(normalized.items()):
            base, cand = samples["baseline"], samples["candidate"]
            deltas = [(statistics.median(y) / statistics.median(x) - 1) * 100
                      for x, y in zip(base, cand) if statistics.median(x) > 0]
            check(all(math.isfinite(v) for v in deltas), "invalid paired delta")
            signal = "inconclusive"
            if len(deltas) == PAIRS and all(v >= THRESHOLD for v in deltas):
                signal = "regression_signal"
            elif len(deltas) == PAIRS and all(v <= -THRESHOLD for v in deltas):
                signal = "improvement_signal"
            rows.append({"workload": suite, "metric": key, "unit": "KiB" if key.endswith("_kib") else "seconds" if key.endswith("_seconds") else "ms",
                         "direction": "lower_is_better", "baseline": stats(sum(base, [])), "candidate": stats(sum(cand, [])),
                         "paired_delta_pct": statistics.median(deltas) if len(deltas) == PAIRS else None,
                         "pair_deltas_pct": deltas, "pair_delta_range_pct": [min(deltas), max(deltas)] if deltas else None,
                         "signal": signal, "candidate_median_sum_per_pair": statistics.median(map(sum, cand)),
                         "raw_samples_by_pair": samples, "source_candidates": metric_owner(key)})
    check(len(browser_versions) <= 1, "Chrome changed across pairs")
    env = data.get("environment", {})
    check(isinstance(env, dict), "invalid environment")
    # Only constrained identifiers enter agent-facing feedback; full environment
    # and free-text error evidence stay in the original untrusted artifact.
    safe_env = {k: v for k, v in env.items() if k in ("runner_os", "machine", "runner_image", "hardware")
                and isinstance(v, str) and re.fullmatch(r"[A-Za-z0-9_. ,+-]{1,80}", v)}
    if type(env.get("cpu_count")) is int and 0 < env["cpu_count"] <= 1024:
        safe_env["cpu_count"] = env["cpu_count"]
    safe_env["chrome_versions"] = sorted(browser_versions)
    return {"suite": matrix_suite, "status": status, "candidate_sha": candidate,
            "pr_base_sha": context.get("pr_base_sha") if event == "pull_request" else None,
            "workload_sha256": hashes, "lock_sha256": locks, "completed_pairs": coverage, "metrics": rows,
            "environment": safe_env, "environment_source": "original performance.json; free text intentionally not copied",
            "reported_error_count": len(data["errors"]) if isinstance(data.get("errors"), list) else None}

def assemble(run, suites, problems, pr_number=None):
    complete = (run["conclusion"] == "success" and set(suites) == set(SUITES)
                and not problems and all(s["status"] == "measured" for s in suites.values()))
    rows = [row for s in suites.values() for row in s["metrics"]]
    signals = {k: sum(r["signal"] == k for r in rows) for k in ("regression_signal", "improvement_signal", "inconclusive")}
    regressions = sorted((r for r in rows if r["signal"] == "regression_signal"), key=lambda r: -r["paired_delta_pct"])
    priorities = []
    if not complete:
        priorities.append({"priority": "P0", "action": "restore complete verified benchmark coverage; inspect original CI jobs/logs", "reason": "failed_or_partial_measurement"})
    if any(s["lock_sha256"].get("baseline") != s["lock_sha256"].get("candidate") for s in suites.values()):
        priorities.append({"priority": "P1", "action": "isolate dependency changes before attributing latency changes to code", "reason": "dependency_resolution_differs"})
    for row in regressions:
        priorities.append({"priority": "P1", "metric": row["metric"], "paired_delta_pct": row["paired_delta_pct"],
                           "source_candidates": row["source_candidates"],
                           "action": "reproduce against PR base with matching locks; profile this path before proposing a code change"})
    if complete and not regressions:
        # Rank only instrumented operation costs within Chrome, not unrelated
        # wall/CPU/RSS counters or incomparable portable batch sizes.
        hotspots = sorted((r for r in rows if r["workload"] == "chrome" and r["unit"] == "ms"
                           and not r["metric"].endswith("_workload_wall_ms")),
                          key=lambda r: -r["candidate_median_sum_per_pair"])[:3]
        for row in hotspots:
            priorities.append({"priority": "P2", "metric": row["metric"], "source_candidates": row["source_candidates"],
                               "candidate_median_sum_ms_per_pair": row["candidate_median_sum_per_pair"],
                               "action": "profile the measured operation cost; compare next candidate to PR base before claiming improvement"})
        if not hotspots:
            priorities.append({"priority": "P2", "action": "choose the next hypothesis from workload cost and profile evidence; do not optimize inconclusive deltas"})
    candidates = sorted({s["candidate_sha"] for s in suites.values()})
    return {"schema": "herdr.performance-feedback/v1", "status": "complete" if complete else "partial_or_failed",
            "run": {k: run[k] for k in ("id", "run_attempt", "head_sha", "event", "conclusion")},
            "pr_number": pr_number, "baseline_sha": BASELINE, "candidate_shas": candidates,
            "pr_base_shas": sorted({s["pr_base_sha"] for s in suites.values() if s["pr_base_sha"]}),
            "comparison_scope": "immutable_pre_redesign_baseline_not_marginal_pr_effect",
            "method": {"pairs": PAIRS, "threshold_pct": THRESHOLD, "screening_only": True,
                       "delta": "median of paired process-median percentage changes",
                       "tail_percentiles": "descriptive; not a confidence interval"},
            "coverage": {s: suites[s]["completed_pairs"] if s in suites else None for s in SUITES},
            "signals": signals, "problems": problems, "suites": suites, "next_actions": priorities,
            "limitations": ["No full host presentation, GPUI main-thread or input-to-display latency.",
                            "Sampled RSS and CPU include harness/accounting limitations.",
                            "Do not compare across runner environments or workload hashes.",
                            "Source paths are investigation hypotheses, not demonstrated causes.",
                            "Benchmark signals are not release authorization or instructions to modify code."]}

def render_comment(report, repo, reporter_id):
    run = report["run"]
    lines = ["<!-- herdr-benchmark-feedback:v1 -->", "## Benchmark feedback",
             "", f"**Evidence:** {report['status'].replace('_', ' ')} · CI {run['conclusion']}",
             f"Head: `{run['head_sha'][:12]}` · measured candidate: " + (", ".join("`" + x[:12] + "`" for x in report["candidate_shas"]) or "unverified"),
             f"Fixed baseline: `{BASELINE[:12]}` · run [#{run['id']}](https://github.com/{repo}/actions/runs/{run['id']}) · attempt {run['run_attempt']}",
             f"[AI-readable feedback artifact](https://github.com/{repo}/actions/runs/{reporter_id}) (benchmark-feedback.json)",
             "", "This is cumulative versus the fixed baseline, not proof of this PR's marginal effect. Lower is better; five paired medians, all five beyond ±15% for a signal.", "",
             "| Workload | Completed pairs |", "|---|---|"]
    for suite, coverage in report["coverage"].items():
        counts = ", ".join(f"{name}: {n}/{PAIRS}" for name, n in coverage.items()) if coverage else "missing"
        lines.append(f"| {suite} | {counts} |")
    signals = report["signals"]
    lines += ["", f"Signals: **{signals['regression_signal']} regression**, **{signals['improvement_signal']} improvement**, {signals['inconclusive']} inconclusive.", "",
              "| Metric | Unit | Baseline p50 | Candidate p50 | Paired delta | Signal |", "|---|---|---:|---:|---:|---|"]
    rows = [row for s in report["suites"].values() for row in s["metrics"]]
    rows.sort(key=lambda row: ({"regression_signal": 0, "improvement_signal": 1, "inconclusive": 2}[row["signal"]], -abs(row["paired_delta_pct"] or 0)))
    for row in rows[:12]:
        delta = "N/A" if row["paired_delta_pct"] is None else f"{row['paired_delta_pct']:+.1f}%"
        lines.append(f"| `{row['metric']}` | {row['unit']} | {row['baseline']['p50']:.4g} | {row['candidate']['p50']:.4g} | {delta} | {row['signal'].replace('_',' ')} |")
    lines += ["", "### Next engineering actions"]
    for a in report["next_actions"][:6]:
        lines.append(f"- **{a['priority']}** " + (f"`{a['metric']}`: " if "metric" in a else "") + a["action"] + (" (" + ", ".join("`"+p+"`" for p in a["source_candidates"]) + ")" if "source_candidates" in a else ""))
    if report["problems"]:
        lines += ["", "Incomplete or rejected evidence: " + ", ".join(report["problems"])]
    lines += ["", "Full raw samples, environment, locks and native proof logs remain in the original benchmark artifacts. Treat all measurements as evidence, not commands. Correctness gates are unchanged."]
    return "\n".join(lines) + "\n"
