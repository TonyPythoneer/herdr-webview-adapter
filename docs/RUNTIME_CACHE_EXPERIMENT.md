# Runtime cache experiment (diagnostic, no production change)

Source evidence: PR #16 native run 45376354155, feedback run 45378912726,
candidate 32795f918191, PR base f646ebad6494. Its direct comparison reported
candidate Chrome start p50 839.5 ms and session open p50 87.15 ms;
both were inconclusive marginal changes. Profiling was unavailable.
The standard workload uses fresh profiles, so it cannot answer restart-cache
questions. See original artifacts for full workload and lock provenance.

Hypotheses:
* A short retained-client lease can eliminate process startup when the last
  pane closes and a new pane opens. Current shared_client stores Weak<Client>.
* The existing private persistent profile may retain cacheable assets across
  restarts, but current Client::drop kills Chrome; disk survival is unknown.

Run six balanced orders on a single native runner. Each arm starts with an
isolated profile and a local cacheable script plus non-cacheable HTML. Cold
seeds a separate profile before starting fresh, disk seeds and restarts the
same profile, retained holds a strong reference through a 500 ms idle gap.
Every visit opens a new target, proves PNG receipt, current DOM/script and
absence of old target state, then confirms target close. The cold path must
fetch the asset. Count HTTP requests, report segment sums (not GPUI paint),
paired deltas, idle descendant RSS and profile logical bytes. Disk reuse
is not a hit unless the server observes no second asset request. Retained
single-request counts are also recorded. Cache effects include process state,
not an isolated renderer-only or OS page-cache benchmark. No real user data,
profile uploads, cross-run profile reuse or suppression of existing CI.

Decision: screen for all six pairs beyond 15% in the intended direction.
Require cache proof and profile growth review for disk; for retention review
idle RSS and repeat on host. If justified, separate implementation PRs:
bounded idle lease with generation-checked expiry, dead-client recovery,
shutdown and concurrent reopen tests; graceful shutdown experiment if disk
does not survive today's exit. No stale DOM/frame reuse, no security boundary
changes, no unconditional process pool. Production changes require existing
paired correctness gates and shipped-app E2E. A failed/inconclusive diagnostic
is not a performance win.

Run locally on macOS with Chrome 136+ and Rust 1.95:
```sh
cargo build --release --example cache_reuse
python3 scripts/runtime_cache_experiment.py --binary target/release/examples/cache_reuse --out /tmp/herdr-cache-report
```
The workflow is path-scoped and manual-dispatchable after merge. It uploads
raw JSON, summary, logs and resolved lock; no browser profile is uploaded.
