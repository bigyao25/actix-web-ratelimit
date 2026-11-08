# Docker benchmark harness

Run from the repository root. Python 3.10+, Docker Engine with Compose, at least
4 VM CPUs / 4 GiB RAM and cgroup v2 are required for Safe/local exploration; CI smoke supports 2 CPUs. No host ports are published.

```sh
python3 benchmarks/bench.py smoke --variants baseline head --backends memory redis --allow-dirty
python3 benchmarks/bench.py safe --variants baseline v0.2.1 head --backends memory redis
python3 benchmarks/bench.py safe --variants baseline governor --backends memory --modes allow mixed
python3 -m unittest discover -s benchmarks/tests -v
```

Formal runs reject a dirty working tree by default. `--allow-dirty` explicitly marks
results non-reproducible. Commit the harness before producing formal measurements.
`--skip-build` is allowed only for smoke/defect and reuses images when the operator knows they match the source;
image IDs and architecture are recorded. Do not use it for a formal source comparison.

Safe defaults: 15 s warmup, 60 s measurement, 5 repetitions, 1,000 target RPS.
Options: `--clients 1 100 10000`, `--concurrency 32 128 512`,
`--modes allow deny mixed reject`, `--duration`, `--warmup`, `--repeats`, `--rate`, `--output`.
Safe rejects >2,000 RPS, >120 s measurements, >30 s warmup and >10 repeats.
Smoke uses 1 s warmup, 2 s measurement, one repeat, up to 500 RPS, one client, concurrency 32.
A full default matrix is deliberately long. Start with smoke and a selected matrix.

## Resource protection

Loadgen: 1 CPU/512 MiB, server: 2 CPUs/1 GiB, Redis: 1 CPU/512 MiB.
Swap limit equals memory limit, preventing extra swap allocation. Default disjoint
CPU sets are 0, 1–2, 3. Compose environment variables `LOADGEN_CPUSET`,
`SERVER_CPUSET`, `REDIS_CPUSET` can select other *disjoint* CPUs on a shared host.
The runner refuses unsupported limit capabilities and checks container HostConfig.
Redis has 256 MiB maxmemory/noeviction, no persistence, and exists only inside this
run's internal network. Each cell recreates its own Redis and server; no shared
Redis keys are deleted. A checkout lock prevents simultaneous runner invocations.
Cleanup on normal exit, exceptions and SIGTERM only removes the generated project.
SIGKILL/host failure cannot execute cleanup: inspect `docker compose ls`, then run
`docker compose -p <recorded-rlbench-project> -f benchmarks/compose.yaml down --volumes`.
Never use global prune or FLUSHALL. Images/build caches are retained for reuse.

Recommended Docker Desktop VM upper allocation: 6 CPUs / 8 GiB. The tool does not
change global settings. Compilation precedes measurement; builds use two Cargo jobs. Cold image downloads and
builds can take several minutes. Docker Desktop VM scheduling, other containers,
thermal throttling, host activity and power mode can cause noise even with CPU sets.
Other running containers are recorded and warned about, not stopped.

## Workload and correctness

Adapters share the same Actix 4.18.1 HTTP handler, header identity, worker count and
request body. Historical Cargo dependencies use exact published versions, with
independent checked-in locks. HEAD uses the local path. Registry checksums in locks
identify release source archives; HEAD SHA and dirty status identify local source.
No production limiter code is changed. v0.1.2 retains synchronous Redis I/O,
non-atomic checks, off-by-one threshold and timestamp member collisions.
It is not eligible for an equivalent-correctness performance victory.

Warmup uses separate identities. Measurement windows exceed the entire round, so
all requests are inside one window: each client's admitted count must equal
min(attempts, quota). This is a burst-cap oracle, **not** a complete rolling-window
proof. Existing crate regression tests cover boundary/expiration behavior.
Allow quota exceeds scheduled load; deny quota is zero; mixed quota is approximately
half the average scheduled requests per client. With 10,000 clients and sparse
traffic, mixed may admit all observed requests; the report records actual counts.

The generator maintains bounded concurrency and no unbounded request queue.
Dropped scheduled arrivals are recorded separately as in-flight-capacity or scheduler drops; offered RPS means attempted requests
per scheduled duration, achieved RPS includes drain time, target RPS is separate.
Latency starts when the request is actually sent; missed arrivals are reported
rather than hidden in latency percentiles. Python's generator has a limited ceiling;
this safe harness measures latency/resource cost at capped rates, not peak QPS.

## Diagnostics and reports

`environment.json` records host/VM, Docker, image IDs/architecture, Git SHA, dirty
state and settings. Copied Cargo locks accompany `raw.json`, `validated.json` and
`report.md`. Raw rounds preserve CPU cgroup counters/throttling, memory current and
peak/process RSS (`/proc/1/status`), socket counters, Docker stats samples, OOM state and Redis INFO
(memory, clients, CPU, error counters). Do not publish raw inspect/environment files
without reviewing host paths and environment metadata.

OOM, Redis OOM, unexpected statuses, request errors, quota mismatches, scheduler
drops, missing diagnostics and severe measured throttling invalidate measurement quality.
Schema-3 in-flight-capacity drops retain clean overload observations but fail the load level.
Throttling threshold is currently 100 ms per measured second. Inspect raw counters
because startup/warmup also contribute to resource samples. Invalid raw results
remain visible; never treat invalid rows as ranked winners.

Variants alternate forward/reverse order across repetitions. Repeated identical
cells provide A/A range calibration; use >=5 repetitions. Reports provide medians,
ranges, p99, baseline/reference deltas and A/A spread. These are descriptive, not
statistical significance tests. A fixed-rate RPS delta is not a peak-throughput gain.
Keep results from different physical hosts separate; CPU model/VM and quotas matter.

## Extending

Add `adapters/<variant>/Cargo.toml`, `Cargo.lock`, `src/main.rs`, and a CLI registry
entry. Use the shared server where API-compatible; otherwise implement only a thin
middleware adapter with identical HTTP behavior. Generate locks deliberately outside
measurement. Never regenerate locks in Docker builds (`--locked` is mandatory).
See [competitors.md](competitors.md) for verified competitor versions and exclusions.
New algorithms need separate semantics and correctness oracles.

The benchmark workflow runs only on manual dispatch; normal CI does not schedule
long measurements or a regression gate. Current limitations: no full rolling-window
oracle, no remotely induced Redis fault suite, no host thermal telemetry, no
simultaneous confidence guarantees across comparisons, and no multi-worker race
oracle for bucketed competitor admission. BuildKit resource usage is not capped
by Compose runtime limits.

## Timing and load diagnostics (schema 2)

Completion RPS uses at least the entire scheduled interval, plus any drain time.
It cannot exceed offered RPS due to the final pacing gap. Reports also record
completed-in-window counts, actual peak/mean in-flight requests, dispatch lag,
latency including dispatch lag, and separate 200/429/503 latency distributions.
Dropped arrivals remain explicit: scheduler drops invalidate measurement; capacity drops fail the load level without erasing overload observations. Concurrency is a ceiling,
not a claim that all slots were occupied. Loadgen CPU/throttling deltas are recorded
around measurement; server resource samples still include startup/warmup overhead.

Single-round repeat spread is N/A. Repeated completion-RPS spread is not a capacity
noise estimate. Reports warn about short measurements, fewer than five repeats,
and fewer than 10,000 samples per round for p99. Old schema-1 data retain their
original numbers and receive a legacy-timing warning; they are not silently corrected.

Use a sequential rate sweep within the existing safe cap, and inspect loadgen
saturation/dispatch lag and server utilization before describing a run as high load:

```sh
python3 benchmarks/bench.py safe --variants baseline head --backends memory --clients 1 --concurrency 32 --modes allow --rates 100 500 1000 2000 --plan
# Remove --plan to execute. At the default durations/repetitions this takes 50 minutes of load plus setup/build/cleanup.
```

`--plan` performs no builds or Docker operations. Each rate is a separate report
cell; different rates, durations or warmup settings are never pooled.
A result at the 2,000 RPS safe ceiling may still be far below server capacity.
A higher-ceiling load generator and explicit resource review would be needed for
capacity tests beyond this profile; do not infer maximum throughput from this tool.

## Retained files and cleanup

Runtime containers/networks are removed and cleanup is verified. Local reports,
adapter Cargo target directories, shared Cargo downloads, Docker images and BuildKit
caches are retained intentionally for reuse. No Docker Desktop global settings are
changed. Shared caches cannot be safely attributed to this project alone.

```sh
python3 benchmarks/artifacts.py                    # inventory only
python3 benchmarks/artifacts.py --clean-local-cache # remove only local target/ and __pycache__
```

Cleanup preserves source, lockfiles, results and Docker/shared caches. It refuses
symlinked paths. This tool never invokes global prune, removes unrelated images,
or deletes shared Cargo data. Temporary source archives and logs from manual
investigations under the host temporary directory may be reclaimed separately.

## Capacity exploration and workflow results

A fixed-rate test is useful before saturation: it measures correctness and the
latency/resource cost of serving that offered workload. It does not measure peak
throughput. Increasing the concurrency ceiling alone does not increase offered
load. The local `explore` mode changes the offered rate while keeping Docker
resource limits unchanged:

```sh
python3 benchmarks/bench.py explore --plan
python3 benchmarks/bench.py explore --allow-dirty
```

Default exploration: baseline/HEAD, both backends, one identity, concurrency 128,
1k/2k/4k/8k/10k RPS, 2 s warmup, 10 s measurement, one repeat. Maximums: 10k RPS,
30 s measurement, 10 s warmup, 3 repeats. These are boundary probes, not publishable
rankings. `assessment.md` reports missing bottleneck evidence, generator pressure,
server pressure candidates or latency-budget crossings without claiming causality.
The diagnostic 10 ms p99 budget is a fixed probe threshold, not a universal service
SLO. Generator saturation invalidates server capacity comparisons. Confirm any
candidate boundary using longer repeated runs. The Python generator may hit its
own CPU limit first; increasing offered rate cannot resolve that ambiguity.

The benchmark workflow runs **only by manual dispatch**, selecting **compare, smoke or defect**, never explore. Pushes and pull requests do not trigger it.
CI explicitly uses server=1 CPU, loadgen=0.5 CPU, Redis=0.5 CPU, one HTTP worker;
memory/swap limits are unchanged. On 2-CPU runners the generator and Redis share
CPU 0, and the server uses CPU 1. On >=3-CPU runners sets are disjoint. This sharing
is recorded and never silently applied to local Safe tests. Smoke defaults to
500 RPS, 1 s warmup, 2 s measurement; the workflow is an operational/correctness
check, not a performance regression gate. Only the known historical quota defects
are tolerated; historical OOM, missing metrics and transport failures still fail.

After a run, open **Actions → Benchmark smoke → the run → Summary**:

- The assessment shows execution status, valid/invalid rounds, historical quota
  defects, target/completed RPS, latency, actual concurrency and pressure signals.
- Short-run and algorithm/resource limitations remain explicit; there is no
  fastest-crate ranking. A failed build/preflight produces a no-data assessment.
- The summary contains a direct link to the uploaded **benchmark-diagnostics**
  artifact. It is also available in the run's **Artifacts** section for 14 days.
- The ZIP contains `assessment.md`, `report.md`, `raw.json`, `validated.json`,
  `environment.json`, exact lockfiles and `run.log` (when produced). If no raw
  data exist, start with `run.log`. Upload/summary steps run even after failure.

`GITHUB_STEP_SUMMARY` is populated with Markdown; it does not post PR comments,
change repository permissions or publish a website. To preview the same assessment
locally:

```sh
python3 benchmarks/summary.py benchmarks/results/<run> --status completed --output benchmarks/results/<run>/assessment.md
```

Runner architecture, quotas and platform identifiers must match before comparing
runs; local Apple Silicon results cannot rank against shared x64 Runner numbers.

## High-load comparative benchmarks (manual only)

Select **Actions → High-load Benchmarks → Run workflow → suite: compare**. This is the default
manual suite. Pushes and pull requests never trigger this workflow. The obsolete
low-load behavior suite has been removed. Optional `smoke` is only an operational
check, not the competitor comparison result.

The workflow uses a two-stage campaign per backend (see below). The fixed-matrix
`bench.py compare` entry point remains available: baseline, HEAD, governor, trypema
and extensible on the same Actix 4.18.1 stack. Its defaults are Memory 30k/60k and
Redis 15k/20k/25k offered RPS, 512 slots, one client, all-allow, 5 s warmup + 30 s
measurement and five alternating-order repeats. Unsupported variants are filtered
before building. This optional fixed matrix has 110 cells / 3850 seconds of load;
the workflow instead selects confirmation cells from same-runner short probes.

high-load comparison requires **four native Docker CPUs**: native Rust loadgen=2 CPUs (sets 0,1),
server=1 CPU (set 2, one worker), Redis=1 CPU (set 3). Memory/swap limits remain
512 MiB/1 GiB/512 MiB. It refuses smaller/shared layouts, including CI smoke's
0.5-CPU generator. Docker Desktop global configuration is unchanged. Existing Safe
resources and rate bounds remain unchanged. The explicit high-load comparison hard ceilings are
100k RPS, 512 concurrency, 60 s measurement, 15 s warmup, five repeats.

```sh
python3 benchmarks/bench.py compare --plan
python3 benchmarks/bench.py compare
# A narrower, repeated Memory comparison:
python3 benchmarks/bench.py compare --backends memory --variants baseline head governor trypema extensible --rates 10000 30000 60000 --plan
```

The Rust/Tokio generator uses pooled HTTP/1.1 connections, bounded in-flight tasks,
fixed-size latency histograms, and explicit dropped-arrival/error accounting. Its
locked dependencies are copied into the run artifacts. Histograms round upward:
0.01 ms bins through 100 ms, 0.1 ms through 1 s, then 1 ms. The Python generator
remains for smoke/Safe/explore compatibility; it is not used by high-load comparison. Native image
architecture is verified along with server images. No x86 emulation is requested.

Fixed resource throttling is normal when pushing a capped server: high-load comparison records
`quota_throttled` and raw cgroup counters without invalidating a round solely for
SUT quota throttling. OOM, Redis OOM, malformed responses, scheduler drops,
transport failures and asserted quota violations still invalidate measurement quality.
Schema-3 capacity-only drops fail load qualification and are excluded from matched cost ratios; they do not invalidate measurement quality.
Generator pressure prevents attributing an observed ceiling to the server.
Before the probe phase, the campaign runs up to two short baseline-only probes at
the top of the default ladder. If the generator reaches 70% of its own CPU quota
or misses scheduled arrivals, the whole ladder is scaled down so the top rate
lands at that headroom; the decision is recorded as `calibration` in
`campaign-plan.json`. Explicit `--probe-rates` are never rescaled. A saturated
generator is a harness limit, so it must not consume the campaign or be reported
as a server ceiling.

The assessment's **High-load benchmark qualification** table requires five valid repeats of at least 30 seconds,
>=99% completion of offered rate, scheduled p99 <=10 ms, and generator CPU below
85% of its quota. It reports the highest *qualified tested level*, not an unbounded
maximum. The 10 ms budget is a stated comparison criterion, not a universal SLO.
If every level qualifies without server pressure, raise rates within the explicit
ceiling rather than calling the last number maximum throughput.

Cost tables include competitor/HEAD medians at matched quotas/resources. Policy
labels and nominal over-admission remain visible. Different policies may be
compared as end-to-end products but do not become equivalent algorithms. A winner
is not manufactured from differing accepted-request counts or generator overload.
Old Actix 4.11 results must be rebaselined after the common HTTP stack update.

## Connection reliability and schema 3

Every benchmark server now uses the same explicit **120-second Keep-Alive**.
The native pool uses FIFO reuse, a **4-second idle limit**, and a nonblocking
idle-socket check before sending. Retired idle sockets are counted separately
from request errors. A peer can still close after the check: any error after
sending remains a real failed attempt, **without automatic replay**. This avoids
double quota consumption and prevents retries from hiding errors.

Raw data now include `error_categories` (connect/write/status/headers/body/framing/
timeout), up to 16 error examples with phase-relative timestamps and reuse context,
and pool creation/reuse/idle-retirement counters. Timeout reuse context is unknown
and serialized as null. Counts remain complete even after the example limit.

Execution status, measurement quality and load qualification are separate. A
capacity-only drop is a clean overload observation, not proof that generator CPU
is saturated. Such rounds remain in diagnostic tables but do not qualify that
rate or enter matched-workload cost ratios. Scheduler loss, HTTP/transport errors,
OOM and incorrect counters still invalidate measurements. Schema-2 legacy data
retain their original strict interpretation and missing error details are explicit.

`http_rps` counts completed HTTP responses; `admitted_rps` counts HTTP 200. Legacy
`achieved_rps` remains completed attempts including errors for compatibility; the
assessment displays these separately and qualifies rates using HTTP completion.
Schema-3 accounting checks run even when transport errors are present.

high-load comparison stops after persisting diagnostics if a baseline measurement is invalid. It
does not stop merely because the baseline hit the in-flight bound. To validate
the client lifecycle before a full matrix, run a narrow high-rate calibration:

```sh
python3 benchmarks/bench.py compare --variants baseline --rates 10000 --duration 20 --warmup 5 --repeats 3 --plan
# Remove --plan to execute. Review error categories/pool counters before full high-load comparison.
```

Regression tests cover a remote five-second idle close, earlier idle EOF,
post-write EOF without replay, chunked framing and bounded error examples.
This addresses a source-identified failure mode; a clean local run does not prove
that every error from a previous GitHub run had the same cause.

## Interpreting the connection regression

The four 20-second, 10k RPS baseline/HEAD Memory/Redis cells validate the revised
client lifecycle, not a competitor ranking: zero transport/scheduler errors were
observed locally. Two cells still reached the in-flight bound; one repeat cannot
establish a sustainable rate or equivalence. Archived artifacts retain their old
profile labels; new CLI and workflow runs use `compare` and resource `comparison`.

Before a full repeated comparison, distinguish reserved request slots from active
HTTP exchanges: the driver acquires a semaphore permit before spawning a task,
while `peak_in_flight` counts only started tasks. Capacity drops with peak below
128 therefore can reflect executor backlog, not 128 simultaneous network requests.
Reserved/queued peaks, executor wait and submission burst sizes are now recorded.
Normalize pacing before interpreting rare capacity drops as a server limit. Do
not loosen the qualification threshold simply to produce a winner.

Then calibrate baseline on the same runner for >=20 seconds, sweep 128/512 slot
bounds, and locate the Redis boundary with finer 10k/15k/20k/25k rates. Confirm
candidate levels with >=5 repetitions and phase-aligned resource samples. Memory
and Redis need independent rate ladders; the same coarse levels need not bracket
both boundaries. A clean committed source tree and matched environment are required
for formal comparative conclusions.

## Faster runs without weakening the assertions

The optional fixed comparison uses backend-specific rate ladders: Memory 30k/60k and
Redis 15k/20k/25k, preserving five repetitions per cell. These target the ranges
observed in earlier probes. This is 110 cells rather than 135 (18.5% fewer cells
at matched durations). With the current 30+5 second defaults it is 3850 seconds
of load. Earlier 20+5 second runs were 2750 versus 3375 seconds. Startup/build time
is additional; the workflow campaign uses a data-dependent confirmation subset.
All-allow policy, quotas and resource limits remain identical across peers within
each backend. Existing repeat, transport, SLO and accounting requirements remain.
The shared comparison slot ceiling is now 512 rather than 128, allowing more
short-lived overlap during scheduling stalls. It remains a hard bound, not 512
permanently active connections. Results with different ceilings are separate
workloads and must not be combined into a single performance ratio. Local
baseline/HEAD short calibration completed without drops at this ceiling; sustained
rates still require the unchanged five-repeat qualification on the target runner.
Use `--rates` to override both ladders, or `--memory-rates` / `--redis-rates`
to select them separately; the resolved plan and original CLI inputs are recorded.

Docker stats now streams concurrently into a run-scoped temporary file, rather
than blocking measurement completion in a no-stream polling loop. The sampler is
terminated and reaped before container cleanup. Loadgen startup uses `--no-deps`
only after server/Redis readiness has already been checked. Server/Redis stop grace
is two seconds after requests, counters and diagnostics have completed. This
reduces idle orchestration time, not measurement duration or warmup.

### Historical version defect proof

v0.1.2 is excluded from normal Safe/smoke/comparison matrices. Select manual
workflow suite **defect**, or run:

```sh
python3 benchmarks/bench.py defect --resources ci --plan
python3 benchmarks/bench.py defect --resources ci
```

The fixed proof uses one client, six sequential requests, a quota of five and a
long window. It runs once on Memory (one-second load). It succeeds only when the
known quota violation is actually reproduced without other invalidity. No repeated
performance claims or heavy historical Redis run are required. v0.2.1 remains
selectable explicitly but is also omitted from routine smoke. Existing historical
archives remain untouched.

### Driver queue visibility

`peak_reserved_slots` includes queued and running tasks; `peak_queued_tasks` and
`executor_wait` isolate pending tasks. `submission_lag` isolates pacing delay.
`peak_dispatches_per_1ms_bin` reveals catch-up bursts, while `peak_in_flight`
continues to describe started exchanges. The driver limits not-yet-started tasks
to sixteen and yields to ready socket tasks during catch-up. Waiting preserves
the original scheduled timestamp and the 100ms scheduler-loss check: delay is
visible in submission/scheduled latency rather than hidden by rescheduling.
These observations must not be confused
with server-side admission timestamps or interpreted as a server saturation proof.

The obsolete low-load behavior test was removed; protocol/framing, true five-second
idle-close, error accounting, resource limits, compatibility and cleanup safety
tests remain. Their duration is small relative to the benchmark matrix.

### Contention versus isolated performance

Concurrent peers are useful as an independent contention/resilience experiment.
For attribution, prefer separate containers under a shared total budget, with
per-peer quotas and metrics, over placing every product inside one container.
A shared-container OOM/scheduler failure cannot reliably identify a single product
or establish isolated implementation performance. Such an optional contention
suite is not implemented here and must have a separate report and failure model.

## Actions matrix and scoped workload winners

The manual `compare` workflow now runs **Memory and Redis on two independent
GitHub-hosted runners**, with `max-parallel: 2` and `fail-fast: false`. Each runner
keeps baseline, HEAD and all backend-compatible competitors together. Products,
rates and repetitions are never dispatched to different machines for a direct
comparison. A backend failure retains its diagnostics and does not cancel its
sibling. `smoke` remains one combined job; `defect` remains one Memory proof.

Parallel backends can reduce elapsed time but do not promise a twofold speedup or
lower billed runner minutes: cold builds, queueing and unequal work remain.
Tool tests run once in the preparation job. Redis omits governor before building.
Raw artifacts have unique names `benchmark-diagnostics-memory` and
`benchmark-diagnostics-redis`; `benchmark-overview` indexes their separate reports.
Missing artifacts or failed shards make the overview fail explicitly. No raw
rounds or RPS distributions are pooled across runner/platform identifiers.
See the [Actions matrix documentation](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/run-job-variations).

Each backend follows this bounded campaign **on the same runner throughout**:

1. Probe all supported products with 2-second warmup, 5-second measurement and
   one repeat. Start with Memory 45k/60k or Redis 10k/16k RPS, then refine the
   observed pass/fail bracket. Midpoints are quantized to 500 RPS; stop when the
   gap is <=5k for Memory or <=2k for Redis. Up to three additional short rates
   are allowed, prioritizing HEAD, then other peers. Missing lower/upper endpoints
   can trigger a bounded range extension; invalid or generator-limited rows never
   become capacity-failure evidence. `--resolution` and `--refine-budget` are bounded
   CLI overrides. This is adaptive selection, not a fixed four-level sweep.
   These are high-load probes, not qualified performance outcomes. Invalid HTTP
   baseline measurements stop the campaign; a clean overload level is retained
   but cannot calibrate that offered rate.
2. Confirm at most HEAD's adjacent passing/failing candidate endpoints with all
   supported peers present. Avoid repeating every peer's individual probe rate.
   Non-monotonic HEAD evidence stops selection. Probe bounds, refinement decisions,
   unresolved products and workload cases are saved in `campaign-plan.json`.
   `search_status` records target/observed probe gaps and budget exhaustion; reaching
   the budget does not count as reaching the requested resolution.
3. Confirm selected cells using 5-second warmup, **30-second measurement, five
   repetitions**, alternating product order, identical slots/quotas/resources
   and fresh per-round state. Every confirmation includes baseline and HEAD.
   The first phase builds every adapter and the generator from locked source.
   Subsequent phases use `--reuse-build-manifest`: current SHA, all source hashes,
   Rust/build settings, image IDs, OS and native architecture must still match.
   Source/image drift stops the campaign. This avoids repeated builds and is
   stricter than unchecked `--skip-build` (still forbidden for formal compare).
4. Keep probes and confirmations labeled in raw data. Only confirmations can
   establish qualified levels or directional cost results. A passing top probe
   does not establish an unbounded maximum; an unbracketed boundary stays unknown.
5. Exercise the representative cardinality/response matrix below at a conservative
   shared offered level (80% of the common passing probe, or 80% of the lowest
   available passing candidate if none is common, with a 5k floor). Each case gets one short survey, at most
   one lower-rate fallback, then the same 30-second/five-repeat confirmation.
   Invalid or insufficiently exercised products are explicitly marked/skipped
   rather than wasting long repeats; missing baseline/HEAD coverage fails the
   campaign. A fallback at the same 5k floor is not repeated.

Default representative matrix:

| Measured clients | Mode | Purpose |
| ---: | --- | --- |
| 1 | allow | Hot-key capacity bracket and reference workload |
| 100 | allow | Isolate cardinality from the hot-key case |
| 10,000 | allow | High-cardinality state/lookup cost |
| 100 | mixed | Compare response paths at fixed cardinality |
| 100 | reject | Mostly rejected requests with positive quotas |

`reject` uses about 1% nominal admissions (minimum quota one), so governor and
trypema participate without an artificial zero quota. `deny` remains the optional
legacy zero-quota case. Mixed quota targets half of per-client attempts. Counts
are observed: mixed requires 20%-80% 429s; reject requires >=90% 429s. If a large
client population needs more requests to reach that rejection fraction, its short
survey is extended within the existing 60-second ceiling. Every requested identity
must actually be observed. Baseline ignores quotas and always permits requests.

This five-case design isolates the two main factors; it does not cover every
interaction. `--full-matrix` (or manual dispatch input **full_matrix**) opts into all
nine combinations, including high-cardinality rejection. Both modes retain five
long repeats rather than reducing statistical quality to fit more cases. Warmup
uses separate identities which can remain resident alongside measured identities;
RSS/Redis bytes include warmup/readiness state, not a pure per-measured-client size.

With five supported Memory products the default has **at most 150 long rounds /
5250 seconds of warmup+measurement**, versus 120 / 4200 for the four Redis products.
These are upper bounds; missing bounds and skipped invalid cases reduce work.
Short probes, startup/diagnostics/cleanup and the first cold build are additional.
The expanded workload coverage costs more than the former all-allow-only campaign,
but avoids multiplying nine cases by every rate in a fine sweep. `--plan` exposes
the case list and round/load bounds before Docker runs. Runner timeouts are 180
minutes for representative coverage and 240 for the explicit full matrix; these
are cancellation limits, not promised runtime.

```sh
python3 benchmarks/campaign.py --backend memory --output benchmarks/results/memory --plan
python3 benchmarks/campaign.py --backend memory --output benchmarks/results/memory
python3 benchmarks/campaign.py --backend redis --output benchmarks/results/redis
# Opt into all nine interactions, or tune the bounded search:
python3 benchmarks/campaign.py --backend redis --full-matrix --resolution 1000 --output benchmarks/results/full-redis --plan
# Short orchestration check, never a qualified result:
python3 benchmarks/campaign.py --backend memory --check --allow-dirty --output benchmarks/results/campaign-check
```

Native measurement pauses after warmup and after request completion while the
controller reads server/Redis cgroup counters. `resource_phase=measurement`
excludes warmup; `resource_capture_skew_seconds` records the remaining bracket
overhead. CPU denominators use the native measured interval. This is a bracketed
measurement, not perfectly simultaneous nanosecond sampling. Memory peak remains
a container-lifetime peak; RSS and Redis memory remain snapshots.

`verdicts.json` and **Workload verdicts** separate two questions:

- **Sustained-load winner:** one product passes all five clean confirmations at
  an offered rate where the peer fails load criteria in every repeat, with no
  generator CPU pressure. Mixed pass/fail, invalid measurements and unmatched
  settings yield `inconclusive`. This establishes a win at that tested workload,
  not a universal algorithm-efficiency or absolute maximum-throughput claim.
- **Latency/CPU winner at jointly qualified rates:** peer/HEAD repeat pairs use
  approximate 95% t intervals on log ratios. Five independent block effects are
  assumed approximately normal; with five observations this is limited evidence,
  not a simultaneous significance guarantee across all comparisons. A directional
  result needs the entire interval beyond the larger of 5% and the matching
  baseline's A/A relative range. Missing calibration or unaligned CPU measurements
  or CPU capture skew above 2% of the measurement interval yields `inconclusive`.
  RSS snapshots are not ranked.

**Confirmed passing and failing bounds** lists each product's highest confirmed
passing level, lowest confirmed failing level, gap and censoring/non-monotonic
state for each workload. Short probe intervals are separate from these confirmed
bounds. If an endpoint fails long confirmation, the report does not manufacture a
stable range from short probes or silently run an unlimited search. Intermediate
untested rates are not claimed to have passed.

All-allow comparisons require every HTTP response to be allowed: unexpected 429s
fail the workload even for algorithms without an equivalent sliding-log oracle.
Different policies still retain their own guarantees. Reports may legitimately
show no winner; selecting a favorable rate or ignoring errors cannot create one.
Mixed/reject cases also require peer/HEAD rejection fractions to match within two
percentage points for directional results. Otherwise ratios remain descriptive and
the verdict is inconclusive, preventing cheaper rejection from masquerading as an
implementation win. The workload coverage table exposes requested/observed IDs,
200/429 totals, rejection-fraction ranges and asserted first-window admission counts.
