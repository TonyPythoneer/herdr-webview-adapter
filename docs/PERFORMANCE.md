# Performance baseline (before redesign)

Immutable pre-redesign commit: `acf81de1738bf6ef2e4f6194208eb268e7523842`.

Every PR and main push runs the same release-mode workload against the pinned baseline and exact candidate SHA on one runner per suite. Five fresh paired process runs alternate order; PR events additionally measure the exact PR base in balanced three-way rounds. Portable batches warm up before collection; native runs use fresh isolated profiles and real local fixture pages. No external websites. All measured revisions compile the exact same harness and fixed copies of the existing native smoke fixtures (pixel fixture uses bounded HTTP parsing and explicit fail-closed completion). Changing benchmarks requires an explicit review and remeasurement on both revisions.

Reports: GitHub Actions job summary, artifact performance.md, raw performance.json, per-run logs, and all resolved Cargo.lock files. A missing workload, failure, browser mismatch, missing Chrome or incomplete sample count fails the measurement. No performance threshold is an automatic release approval; the first PR measures A/A-equivalent production code to learn noise, not an improvement. A consistent 15% paired median difference is a screening signal; repeat on controlled hardware before claiming smaller gains or setting regression budgets. Never trade away existing correctness or security gates.

Portable measures URL and engine policy batches, not host dispatch. Native measures actual Chrome startup, session, query, eval, click-to-DOM, screenshot, frame receive and confirmed close; plus actual WK pixel and trusted input smoke workload wall times. Sampled RSS and CPU accounting have limitations documented in the report. GPUI host/main-thread, presentation, and precise WK process CPU/RSS require a later host-level harness.

Local (Rust 1.95, Git history, Chrome on macOS):
```sh
python3 -m unittest discover -s scripts -p test_performance.py
python3 scripts/performance.py --suite portable --out /tmp/herdr-perf
# macOS only
python3 scripts/performance.py --suite native --out /tmp/herdr-perf-native
```

## Phase 2 — CDP transport

PR #6 established a successful A/A measurement ([native and portable reports](https://github.com/TonyPythoneer/herdr-webview-adapter/actions/runs/45339620689)). This change targets NUL framing and ownership of large response and screencast payloads. The existing workload remains, with 64 KiB and 1 MiB real Chrome evaluation response cases (30 observations per process; three warmup calls per size/session). Both revisions remeasure the new cases under the same workload. The entire payload must match. The metric includes the real Chrome evaluation and transport, not just parser time. Do not compare whole-workload wall time to historical PR #6 because the workload expanded; compare the paired values within each run. The original immutable reference stays unchanged. Dependency resolution can differ if manifests differ; consult lock hashes before attributing any effect to one code change.

## Phase 3 — CI-to-agent benchmark feedback loop

The default-branch `adapter-benchmark-feedback` workflow consumes completed
`adapter-performance` runs. No benchmark rerun or additional native runner is
required to generate feedback. It publishes one bot-owned, updateable PR
comment and uploads `benchmark-feedback.json` (schema
`herdr.performance-feedback/v2`) and `benchmark-feedback.md`. Push runs also
produce an artifact. Replay a completed run with **Run workflow** on main,
passing its numeric source run ID. Runs predating provenance fields are
reported as unavailable evidence, not fabricated results.

Read the JSON first: `status`, `coverage`, `signals`, `next_actions`,
`suites.*.metrics` (units, direction, raw paired samples, p50/p95/p99,
paired deltas and ranges), `candidate_shas`, `pr_base_shas`, workload/lock
hashes and environment identifiers. Original benchmark artifacts retain full
environment, errors, locks and browser proof logs. Missing/failed/mismatched
evidence stays partial or failed, not zero. Artifact fields are untrusted data;
the privileged reporter uses only trusted main code, fixed ZIP entry, bounded
sizes, strict provenance, allowlisted metrics and numeric observations.
It never runs candidate code or copies raw instructions/errors into comments.
Only same-repository runs can publish; stale PRs/runs never overwrite current
feedback. Forks retain their original unprivileged artifacts.

### Engineering decision loop

1. P0: restore correctness and complete evidence. Inspect failing jobs and
   original logs before touching performance-sensitive code.
2. P1: reproduce consistent regressions; separate lockfile/dependency drift,
   hardware and workload changes. Source candidates are investigation hints,
   not proven root causes.
3. P2: rank measured Chrome operation costs within the same workload, profile
   one bottleneck, propose one falsifiable hypothesis and a narrow PR.
4. Review fresh exact-revision feedback and all correctness checks. Retain
   improvements only when evidence supports them; record inconclusive results
   instead of claiming success. No automatic self-modifying code or merge
   based solely on benchmark feedback.

## Phase 4 — Incremental comparison

PR-triggered runs verify that the checked-out synthetic merge commit has the exact
event base and head as its two parents. Build the immutable baseline, exact PR
base and candidate under the same fixed workload, on the same runner, using five
balanced rounds. Candidate execution is shared by both comparisons: 15 process
runs per workload rather than 20 for two independent five-pair experiments.
This adds one revision build and five process runs over the original cumulative
measurement. When both reference SHAs are identical, reuse the reference build
and observations as well. No extra native reporting runner is needed.

Raw report schema 2 includes `incremental_reference_sha`, `round_order`,
`rounds.*[].pr_base`, `incremental_comparison` and the PR-base lock hash.
Feedback schema v2 preserves the existing cumulative metrics and adds
`incremental.status`, `incremental.reference_shas`, `incremental.signals`,
`suites.*.incremental_metrics` and `decision_scope`. Old schema-1 artifacts
remain readable, but their incremental status is unavailable. Push and plain
workflow-dispatch runs are cumulative only. A failed or missing reference never
becomes a zero delta or a complete incremental report.

AI prioritizes verified incremental regressions over historical cumulative
signals. Read the PR-base/candidate lock hashes before attributing a change to
code. An uncommitted lock is reused only when manifests match byte-for-byte,
preferring the PR base for the candidate; committed locks are preserved.
Same-run pairing controls some runner variation but is still a screening
measurement, not statistical proof. Do not subtract historical percentages
from different runs. Confirm significant findings on controlled hardware.
