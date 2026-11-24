# Performance baseline (before redesign)

Immutable pre-redesign commit: `acf81de1738bf6ef2e4f6194208eb268e7523842`.

Every PR and main push runs the same release-mode workload against the pinned baseline and exact candidate SHA on one runner per suite. Five fresh paired process runs alternate order. Portable batches warm up before collection; native runs use fresh isolated profiles and real local fixture pages. No external websites. Both revisions compile the exact same harness and fixed copies of the existing native smoke fixtures (pixel fixture uses bounded HTTP parsing and explicit fail-closed completion). Changing benchmarks requires an explicit review and remeasurement on both revisions.

Reports: GitHub Actions job summary, artifact performance.md, raw performance.json, per-run logs, and both resolved Cargo.lock files. A missing workload, failure, browser mismatch, missing Chrome or incomplete sample count fails the measurement. No performance threshold is an automatic release approval; the first PR measures A/A-equivalent production code to learn noise, not an improvement. A consistent 15% paired median difference is a screening signal; repeat on controlled hardware before claiming smaller gains or setting regression budgets. Never trade away existing correctness or security gates.

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
