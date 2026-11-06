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

## Competitor expansion (not yet container-verified)

Added actix-trypema =0.1.2/core =2.2.1 and actix-extensible-rate-limit =0.4.0
Memory/Redis adapters. All seven adapters now pin Actix =4.18.1. All seven passed
locked offline cargo check; the new adapters also passed direct dependency/API
checks. Extensible emits a future-incompatibility warning; source is unmodified.
Tool regression tests cover capability filtering, trace scheduling, matched-cost
groups and evidence labels. Behavior fixtures in unit tests are synthetic and
are not measured competitor outcomes.

The requested Docker build/run was rejected at authorization, so no new container
benchmark ran and no newly measured competitor differences are claimed. Existing
4.11.0 results remain historical and must not be compared as limiter improvements
to this updated common HTTP stack. Workflow dispatch now offers the bounded
behavior suite; manually dispatched CI smoke adds both new competitors. The
benchmark workflow has no automatic push or pull-request trigger.

## High-load comparison revision

Low-load behavior entry points were removed. Manual Workflow defaults to `compare`,
using a native Rust/Tokio pooled HTTP/1.1 generator with bounded tasks/histograms.
Default rates are 10k/30k/60k, five repeats; resources are disjoint 2/1/1 CPUs for
loadgen/server/Redis. Safe/smoke resource defaults remain available separately.
Native protocol/histogram/zero-warmup tests pass (3); Python tool tests pass (32).

A native 10k RPS end-to-end check completed 9 cells (3-second measurements, one
repeat): 8 valid; extensible Memory dropped 111 scheduled arrivals at the in-flight
cap and was excluded. Its loadgen CPU did not establish generator CPU saturation;
no causal server bottleneck or performance winner is claimed. Every other cell
completed the planned 30,000 requests. This is a high-arrival-rate integration
check, not the full repeated high-load comparison qualification. Full defaults remain manual-only.

In high-load comparison, intentional SUT CPU-quota throttling is retained as a pressure signal, not
solely an invalidity reason. Request loss, protocol/transport errors, OOM and
asserted quota errors still invalidate. Qualification is scoped to matching
backend/client/concurrency/mode and tested rates; generator pressure blocks it.

## Connection lifecycle and reporting regression

Native schema 3 uses FIFO idle reuse, 4-second idle retirement, pre-write idle
socket checks, complete stage/kind error counters and bounded timestamped examples.
There is no automatic request replay. Every benchmark adapter now explicitly uses
120-second server Keep-Alive. Production limiter sources and dependencies are unchanged.
Seven native tests pass, including a real five-second peer idle-close test, earlier
EOF retirement and classification of post-write EOF without replay. Tool tests
cover error/drop accounting even on failure and baseline fail-fast decisions.

A native 10k RPS regression used 5-second warmup and 20-second measurement for
baseline/HEAD on Memory/Redis. All four rounds had zero transport errors and zero
scheduler drops. Baseline Memory and HEAD Redis each returned all 200,000 HTTP
200 responses. HEAD Memory and baseline Redis had 22 and 19 capacity drops,
respectively: clean measurements but failed load levels, not qualified winners.
This is local lifecycle evidence, not proof of the root cause of every earlier
GitHub failure or a replacement for five-repeat comparison on that runner.

Measurement quality and load outcomes are now separate. Capacity-only drops in
schema 3 remain visible and exclude the rate/cost comparison without invalidating
the measurement. Errors, scheduler loss, OOM and bad accounting still invalidate.
Old schema-2 raw results are not rewritten. high-load comparison persists and aborts if a baseline
measurement is invalid, preventing a long matrix after failed client calibration.

## Driver queue, runtime and legacy-proof follow-up

New measurements separate reserved slots, not-yet-started tasks and active HTTP
exchanges; submission/executor wait histograms and fixed-1ms dispatch counts make
client bursts visible. An initial 30k RPS baseline/HEAD Memory probe observed
queued peaks of 120/97. The driver now bounds that queue at sixteen while retaining
original due timestamps, the scheduler-loss check and complete drop accounting.
No request retries or limiter changes were introduced.

With the bounded queue and 128 slots, two 10-second Memory cells had zero
transport/scheduler errors; baseline completed all 300,000 requests and HEAD had
18 capacity drops. A separate 15k RPS Redis probe observed capacity drops even
on the HTTP baseline. These are diagnostics, not proof of a limiter capacity.

The subsequent baseline/HEAD calibration used the same 512-slot bound for all
peers: Memory 30k RPS and Redis 15k RPS, two-second warmup and ten-second
measurement. All four cells returned every scheduled request: 900,000 HTTP 200s
in total, zero errors, zero capacity/scheduler drops, no OOM. Queued peaks stayed
at sixteen. This bound is now the comparison default. Single local repetitions,
dirty working trees and different slot bounds do not establish a speedup,
capacity ranking or equivalence to earlier 128-slot measurements.

Scoped streaming Docker stats samples parsed as JSON after removal of terminal
control sequences. Docker inspect confirmed disjoint CPU sets, 2/1/1 CPU quotas
and 512MiB/1GiB/512MiB memory/swap bounds for loadgen/server/Redis. Containers and
networks created by these probes were removed; images, build caches and ignored
result files remain for reuse. Production sources, crate dependencies and normal
CI configuration are unchanged.

The single historical proof sent six sequential Memory requests with quota five:
v0.1.2 admitted six, zero transport errors. The expected quota/correctness defect
was reproduced and excluded from performance comparison. Normal matrices omit
this version; manual workflow suite `defect` runs this one-second proof once.

The default comparison is now 110 rounds / 2750 seconds of planned load, versus
135 / 3375 previously: 18.5% less load time without removing five repetitions.
Memory uses 30k/60k and Redis 15k/20k/25k; these remain tested offered levels, not
known capacities. Continuous stats, dependency-free loadgen startup after health
checks and shorter post-measurement stop grace reduce orchestration waits.
Only the obsolete low-load behavior assertion was removed; protocol, real idle
close, accounting, resource, report and cleanup checks remain. Forty-two Python
tool tests and seven native generator tests passed. The existing crate suite
also passed: 38 unit/integration/doc tests, with 14 tests skipped by their original
ignore attributes (including external-Redis tests).
Workflow remains manual-only and passed actionlint. A full repeated competitor
matrix on GitHub has not been rerun in this follow-up.

## Backend matrix, probe/confirmation campaign and workload verdicts

The manual workflow now prepares tooling once, runs Memory/Redis as two bounded
independent hosted-runner jobs, and indexes uniquely named artifacts without
pooling raw measurements. Products and repeat blocks remain serial and colocated
within each backend. One shard failure preserves its artifacts and leaves the
other running. Smoke and the one-shot legacy proof each remain a single job.
Actionlint passes; the remote matrix itself has not been executed locally or
dispatched to GitHub.

The campaign probes high offered levels, selects common and product-specific
confirmation candidates on that same host, then uses 30-second measurements and
five repeats. Probe observations never qualify as confirmation evidence. A scoped
sustained-load winner needs five clean passes versus five clean overload outcomes
at a common tested rate; mixed/error outcomes stay inconclusive. Matched cost
results use explicitly limited, approximate paired log-ratio intervals and baseline
A/A noise/practical margins. Unaligned CPU counters or capture skew above 2% block
a directional CPU result. Different algorithms retain their distinct guarantees.

Native phase handshakes now bracket server/Redis counters after warmup and after
measurement, with paused traffic and recorded capture overhead. The collector also
records CPU model and allowlisted GitHub/Runner identifiers. Formal fixed-matrix
defaults now use 30 seconds so their duration meets the confirmation criterion.

Actual local Docker verification used the native 2/1/1-CPU layout:

- Nine 15k RPS cells across baseline/HEAD/governor/trypema/extensible and every
  supported Memory/Redis backend: 1-second warmup, 3-second measurement, all
  405,000 requests returned HTTP 200, zero errors/drops/OOM.
- A real `campaign.py --check` ran baseline/HEAD probing, candidate selection,
  confirmation and collection on Memory: four 2-second cells at 15k RPS,
  120,000 HTTP 200s, zero errors/drops/OOM. Its report explicitly marks operational
  checking, leaves workload verdicts empty and does not qualify its short repeats.
- A final 2-second baseline verified the complete resource-bracket timing and
  Mac CPU-model collection: 30,000 HTTP 200s, zero errors/drops/OOM. Capture
  overhead was recorded rather than hidden in the measurement duration.

All fourteen short cells are diagnostic evidence from a dirty tree, not a
five-repeat competitor result or an observed performance improvement. The seven
native tests passed in the Docker build. Fifty-five Python tool tests pass,
including cross-host/source-change rejection, phase protocol failure, candidate
selection, missing artifacts, unmatched settings and refusing false winners.
Production sources/dependencies, existing project tests and the normal CI workflow
remain unchanged. No rlbench containers or networks remained after verification;
images, caches and ignored reports were retained. No commit, push or workflow
dispatch was performed.

## Representative workloads and bounded capacity refinement

The default campaign now separates cardinality (1/100/10,000 measured clients,
all allowed) from response paths (100 clients, mixed and positive-quota mostly
rejected requests). Full nine-way interaction coverage is opt-in. Requested IDs,
observed IDs, 200/429 fractions and asserted first-window admission totals are
reported. Mixed requires 20%-80% rejections and mostly rejected traffic >=90%.
Non-equivalent response fractions cannot produce directional cost/load results.

Capacity starts at Memory 45k/60k or Redis 10k/16k, with bounded midpoint/range
selection rather than testing every suggested fixed rate. Targets are 5k/2k gaps,
at most three additional short levels by default, and at most HEAD's adjacent
candidate endpoints are promoted to long confirmation. Unbounded/non-monotonic,
budget-exhausted and long-confirmation failures stay explicit. Candidate selection,
resolution status and confirmed intervals have synthetic regression coverage;
these fixtures are not observed capacity data.

Every selected long case retains five 30-second repeats. Runtime is reduced
relative to a full case-by-rate product by choosing representative cases, one
survey/fallback level per additional case, and only boundary endpoint confirmations.
The first phase builds; later phases verify source/build settings and native image
IDs against its manifest. Source or image drift refuses reuse. This avoids phase
rebuilds without using unchecked skip-build. An initial pre-traffic variable-name
collision was fixed; no service containers had started. Cleanup now also avoids
an unnecessary down call before startup or after successful teardown.

Actual local verification on Apple Silicon with the existing bounded CPU/memory
layout:

- All supported competitor Memory/Redis mixed/reject paths: eighteen 10k RPS,
  2-second cells, 360,000 HTTP 200/429 responses, zero transport/errors/drops/OOM.
  HEAD mixed admitted/rejected 10,000/10,000 per cell; positive-quota reject
  admitted/rejected 200/19,800. GCRA's mixed Memory result was 10,300/9,700,
  retained as an observed semantic difference rather than an exact-oracle error.
- Expanded baseline/HEAD campaign check: twenty 15k RPS, 2-second cells covering
  all five representative cases, 600,000 responses, zero errors/drops/OOM.
  All four high-cardinality survey/confirmation cells observed 10,000 identities.
  Nine later phase manifests verified reuse, with no repeated build invocation.
  One baseline high-cardinality confirmation recorded scheduled p99 11.85ms,
  exceeding the 10ms criterion; it remains a clean load failure in the report.
  Short checks produced no qualified capacity/cost winners or confirmed bounds.

Sixty-eight Python tooling tests passed, including bounded search, invalid/CPU-
limited probe rejection, non-monotonic evidence, resolution/budget status,
positive quotas, coverage, response-mix matching and provenance drift refusal.
Actionlint, Python compilation and diff whitespace checks passed. Production
crate sources/dependencies and normal CI are unchanged. No rlbench containers or
networks remained; ignored results and reusable images/caches remain. No remote
long run, commit, push or workflow dispatch was performed. Actual stable capacity
intervals still require the manual long campaign on the target runner.

## Generator calibration after run 42109644774

The manual `compare` run on `f8408fc9` failed in the Memory shard and produced no
confirmed bound in the Redis shard. Both outcomes trace to the harness, not to the
limiter implementations.

- Memory aborted after six rounds. `baseline` at 60k RPS recorded loadgen CPU
  198.2% of a 200% quota, dispatch p99 100.7 ms, 8,918 late drops and 5,950
  capacity drops, so `report.validate` returned `scheduler_drops` and
  `invalid_comparison_baseline` stopped the campaign. The generator was CPU-bound,
  not cgroup-throttled (1,253 us of throttling over 5 s). The 45k probe had already
  consumed 78.6% of the same quota, leaving no headroom for the next step.
- Redis completed 232 clean rounds but confirmed no upper bound. 16k passed for
  every variant; at 31k `baseline` passed 5/5 while `head` passed 3/5,
  `extensible` 1/5 and `trypema` 0/5, all failing on the 512-slot in-flight cap
  rather than server CPU. Mixed outcomes cannot form a bracket, and the 2,000 RPS
  resolution budget was exhausted.

Changes: `capacity.ladder_step` scales the default ladder down when the generator
reaches 70% of its own quota or misses scheduled arrivals, `campaign.py` runs up to
two short baseline-only calibration probes before the probe phase and records the
decision as `calibration` in `campaign-plan.json`, and `bench.py` prints one
diagnostic line per round plus an explicit generator-pressure warning. Explicit
`--probe-rates` are never rescaled.

Verification: seventy-four Python tooling tests pass. Replaying the recorded 60k
round through `ladder_step` yields `[30000, 40000]`.

The first remote attempt (run 42138981032) confirmed the calibration itself: the
60k probe recorded 196.6% of a 200% quota with 16,922 late drops, the ladder
scaled to 39k, and the 39k probe recorded 145.8% with no late drops. That attempt
then failed because the calibration phase selected only `baseline`, so the main
probe phase could not reuse the manifest for the unbuilt peer images. The
calibration phase now selects every peer, which builds all images while only
baseline rows drive the ladder decision.
