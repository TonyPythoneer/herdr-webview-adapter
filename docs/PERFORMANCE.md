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

## Phase 5 — Targeted diagnostic profiling

After every successful native benchmark, choose at most two Chrome diagnostic
probes. Prefer direct PR-base regression signals when available, then rank by
the median accumulated candidate operation time within the fixed workload.
Supported probes: session open/first frame/confirmed close, screenshot PNG,
and 1 MiB evaluation. Startup and other uncovered operations remain explicit
profiling gaps. No production adapter instrumentation or new dependency is
needed: reuse the already-built candidate release example.

Each diagnostic starts a fresh private profile and local fixture, proves the
requested browser operation repeatedly for up to six seconds (bounded count),
and invokes macOS `/usr/bin/sample` on only the owned adapter harness PID for
two seconds at 10 ms intervals. This occurs AFTER the timed rounds; neither
sampler overhead nor diagnostic timings enter cumulative or incremental metrics.
The probe process group has bounded timeouts and cleanup. Raw
`profile-*.sample.txt`, `profile-*.log` and sampler logs remain in the original
native benchmark artifact. Optional `profiling` fields are available in the
raw and feedback JSON, and a bounded section in the PR comment.

The reporter extracts only allowlisted numeric collapsed leaf-stack counts:
wait/sleep, generic read/write syscalls, JSON, base64, allocation, other.
Counts span sampled adapter threads (including idle or blocked threads);
low-count symbols may be omitted by the OS. They are NOT CPU percentages,
Chrome subprocess measurements, UI presentation latency or causal attribution.
The session probe includes open, first frame and close; it cannot isolate
the first-frame stage. Read raw stacks before modifying code. Missing sampler,
failed probe, or malformed optional data is visibly unavailable/partial/invalid
without changing any required benchmark or correctness result. Older reports
remain readable.

Use these diagnostics to choose a falsifiable investigation. For example,
blocked stacks in the frame probe warrant examining wait and protocol timing,
not automatically optimizing JSON; substantial serialization leaf stacks
warrant a focused parser/transport experiment. Measure the proposed change
against the exact PR base; never equate a sampling category with a proven
root cause. Do not add more probes unless they close a specific evidence gap.

## CI runtime optimization

The Ubuntu and macOS correctness jobs and the portable/native performance jobs
already run independently. Cargo compiles independent crates in parallel and
Rust's libtest uses its normal parallel test runner. The expensive part of the
native performance job was three separate dependency builds, not the timed
workloads (PR #10 run 45349169737: roughly 12 minutes preparing/building and
2.4 minutes measuring). The native correctness job likewise spent roughly
8 minutes preparing/building before its unit tests.

Reuse a single Cargo release target directory for all measured revisions,
but explicitly clean the measured package before each build and copy every
release executable into its own verified, executable snapshot before the next
build. Cargo still validates dependency fingerprints and each revision's lock.
Record the binary SHA-256s, preparation/build durations, measurement durations,
and optional profiling duration in the raw artifact. Never reuse measurements.
Native correctness and performance jobs use independent compilation caches,
keyed by OS, architecture, runner image, Rust toolchain and manifest/lock hash.
A cache miss is valid; the first run may populate a cache rather than speed up.
Compare cold and warm timings before claiming an improvement.

Do not parallelize benchmark rounds on the same runner: CPU, memory and browser
contention would invalidate comparisons. Native UI tests also retain their
existing isolation. If measurement time later dominates, split independent
workloads across isolated runners, keeping each workload's three revisions on
the same machine. No correctness or native proof gate is removed.

### All-platform cache layers

Both correctness and benchmark workflows restore Rust registry/git sources,
Cargo target outputs and a bounded 512 MiB local sccache store on both Ubuntu
and macOS. A pinned sccache version is enabled only after successful setup;
compiler stats are logged and the benchmark records JSON statistics before
timed measurements. GitHub cache scopes isolate PR caches from trusted main.
The dev/test and release stores are separated. Cargo still checks its
dependency fingerprints and all correctness tests execute every run.

The benchmark additionally stores exact-revision executables in a
content-addressed cache, keyed by revision SHA, actual resolved lock SHA-256,
all workload/fixture bytes, build/cache scripts, Rust/Cargo/compiler and SDK
versions, OS/architecture/runner image and relevant build flags. A hit verifies
each executable's hash, manifest and executable bit, then snapshots it into
the revision's fresh run directory. A mismatch or damaged entry rebuilds.
Unknown runner images disable persistent binary reuse. Entries are bounded to
eight revisions and 350 MiB total; old entries are evicted. The three immutable
Rust fixture sources and static local HTML are likewise content-addressed,
validated against current bytes and reused in the same archive. No browser
profile, native UI state, external response, test outcome, benchmark sample or
profiling observation is cached.

A first run populates the stores; reruns and subsequent PRs can reuse matching
entries. Record exact cache hit, per-revision binary hits, fixture hits and
stage durations in performance.json and the normalized AI feedback artifact.
Do not compare cache timings to product performance timings. Shared Cargo
target still handles misses efficiently. Correctness and five paired
measurements stay mandatory. The previous cold shared-target run
45355598936 measured baseline 296.196 s, PR base 40.371 s, candidate 33.333 s
(369.900 s total), down from roughly 726 s before shared-target reuse.
Future cold/warm measurements must include cache transfer overhead before
claiming end-to-end speedup.


## Phase 6 — Wake Chrome document readiness on lifecycle events

PR #11 native run 45359588188, measured candidate
`8787e665c4e2e395e4a9572622f89df099599253`: screenshot sampling showed
164 request-wait and 164 pipe-read leaf observations, and the session probe
(35 completed operations) sampled CDP waits and first-frame waits. Neither
establishes parser CPU pressure. Raw artifact 14905066512 contains the stacks.
Workload SHA-256 (workload.rs):
`8d72ba0b1fd474ff43dd03f84a1849acc8b73daef5a9b9896a96d930a7eb70cd`.
PR-base and candidate native lock SHA-256:
`c7fdd477a6b528f029e3318267facd26ad4482210cc58d4f0d9bf959cf29a809`.

Hypothesis: when the first readiness check occurs while a document is loading,
Page.domContentEventFired / Page.loadEventFired can wake the next state check
earlier than a fixed 150 ms sleep. This is a source-supported latency opportunity,
not a proven cause of the baseline p50. The reader uses a bounded one-slot
notification per flattened session; every wake still verifies href and readyState.
The original polling fallback, navigation retry, timeout, ACK path and PNG
format stay intact. No changes to the fixed benchmark harness.

Primary metric: incremental chrome_session_open_ms. Also inspect first-frame
receive, total Chrome workload, CPU/RSS, and correctness. Existing fixture can
already be ready at the first check; an inconclusive result must not be reported
as an improvement. Native correctness includes a delayed-document fixture.
Use the same five paired rounds and existing screening threshold; retain only
if supported by evidence and host E2E. No claim of UI presentation speed.


## Phase 7 — Bounded fast PNG screenshot experiment

Latest main exact SHA `e900f64bbbc9fdf0c210eda18ae6843a28dacefa`,
native run 45370228875, normalized feedback run 45370824491:
screenshot p50 89.483 ms, 1,380.737 ms median cumulative operation time
per process; Chrome startup p50 1,052.768 ms, first frame p50 161.408 ms.
The screenshot diagnostic completed 71 verified operations; 164 wait and
164 read leaf-stack observations are consistent with waiting on Chrome,
not proof of encoder CPU cost. Workload hash
`8d72ba0b1fd474ff43dd03f84a1849acc8b73daef5a9b9896a96d930a7eb70cd`;
candidate native lock `c7fdd477a6b528f029e3318267facd26ad4482210cc58d4f0d9bf959cf29a809`;
fixed-baseline native lock differs. Use the direct PR-base comparison and
check its lock hash before attributing any improvement to code.

Hypothesis: Chromium's documented Page.captureScreenshot optimizeForSpeed
option reduces encoding latency while preserving decoded PNG pixels.
Use it only for known <= 1,048,576-pixel viewports configured at scale 1;
unknown and larger viewports keep the original encoder. Serialize resize
and screenshot, invalidate known dimensions on raw Emulation commands,
retain size checks and retry normal encoding if the fast result exceeds
the existing encoded size cap. Never weaken framing limits. The fast
encoder can increase compressed bytes; no assertion of lower memory use.
Compare exact decoded pixels and large-viewport fallback in native tests.
Official protocol: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-captureScreenshot

Primary metric chrome_screenshot_png_ms; also inspect CPU, RSS, frame and
overall workload. Existing benchmark harness unchanged. No improvement
claim until five paired rounds and correctness gates are reviewed.
Main native correctness run 45370228775 failed the delayed-title assertion;
the fixture now reads complete bounded HTTP headers and verifies script
execution separately from the interactive-document contract.
