# Local verification, 2026-11-05

The local controller is macOS/Apple Silicon, Docker Engine 29.9.2 on native
linux/arm64, Compose 5.6.1, cgroup v2. Existing unrelated containers were recorded
and were not stopped. All measurements used explicitly dirty working-tree mode.
These are tool validation data, not publishable performance comparisons.

- Built and ran baseline, HEAD, published =0.1.2, published =0.2.1 and
  actix-governor =0.10.0 through the same Compose harness.
- Verified applied CPU and memory/swap limits through Docker inspect, collected
  cgroup and Redis diagnostics, and generated real JSON and Markdown reports.
- Nine mixed smoke cells: baseline/HEAD/releases on both Memory and Redis,
  governor on Memory. At quota 100, HEAD and 0.2.1 each admitted 100 of 200 checks;
  0.1.2 admitted 101 in both backends. Both historical cells were excluded for
  over-admission. Governor admitted 103 under its different GCRA semantics and
  is intentionally excluded from equivalent-algorithm comparison deltas.
- A zero-warmup run exposed a division-by-zero error. The runner recorded failure
  and cleaned its project. The issue was fixed and a zero-warmup regression added.
- Seven tool tests pass, including real HTTP counting and socket cleanup.
- Existing `cargo test --locked --all-targets --offline` passes outside the socket
  sandbox. Twelve explicitly ignored real-Redis tests were not run by that command.
- Production sources, root Cargo manifest/lock, and existing CI were not modified.
  A separate short benchmark workflow was added; full performance runs are not CI gates.

Local generated reports live under `benchmarks/results/` (git-ignored). Full
60-second/5-repeat matrices and concurrency 128/512 are implemented but were not
used for the initial smoke verification. Trypema was inspected but not integrated;
see competitors.md. Initial reports use anonymous cgroup memory as an RSS estimate; the final collector reads process VmRSS from /proc/1/status.

Five repetitions of baseline and HEAD on both all-allow and all-deny Memory
workloads completed: 20 short rounds, zero invalid rounds. This validates ordering
and repeated-result aggregation; it is not a long-duration performance baseline.


## Measurement reliability follow-up

Schema 2 adds full-window RPS timing, real request overlap, scheduled/dispatch
latency, per-status latency, and loadgen measurement CPU deltas. Fourteen tool
tests pass, including late completion/drain accounting, CLI safe caps and cache
cleanup preservation. A 100/1000/2000 RPS sweep completed 12 baseline/HEAD cells
on Memory/Redis (5-second measurements, one repeat): no invalid rounds.
At 2000 RPS, HEAD mean overlap was about 0.33 (Memory) and 0.49 (Redis), with
peaks 4 and 8. This is still not a capacity benchmark. Final updated-generator
Memory/Redis smoke also passed. Existing raw reports were retained unchanged.

Runtime containers/networks were cleaned; images, shared BuildKit/Cargo caches,
local adapter targets and reports remain for reuse. `artifacts.py` inventories
local artifacts and offers an explicit local-cache-only cleanup mode.

## CI assessment and local exploration follow-up

Added explicit CI quotas (1 server CPU, 0.5 generator CPU, 0.5 Redis CPU) and a
500 RPS short smoke. Simulating 2-CPU sharing with Redis/generator on CPU 0 and
server on CPU 1 completed four baseline/HEAD Memory/Redis cells with no invalid
rounds. Native host still has more CPUs; only the workload CPU sets/quotas emulate
the smaller Runner. Governor worker configuration compiled after matching the
selected layout. Workflow passed actionlint. Twenty-two tooling tests passed.

Local exploration probes at 4000/8000/10000 RPS completed three short Redis cells
without invalid rounds; at 10000 target RPS, completions were about 9989 RPS.
These 2-second probes did not demonstrate a capacity boundary. No maximum-RPS
claim follows. Job-summary generation was exercised with a local
GITHUB_STEP_SUMMARY file; GitHub-hosted workflow execution/upload has not been run.
