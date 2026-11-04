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
`--skip-build` is allowed only for smoke and reuses images when the operator knows they match the source;
image IDs and architecture are recorded. Do not use it for a formal source comparison.

Safe defaults: 15 s warmup, 60 s measurement, 5 repetitions, 1,000 target RPS.
Options: `--clients 1 100 10000`, `--concurrency 32 128 512`,
`--modes allow deny mixed`, `--duration`, `--warmup`, `--repeats`, `--rate`, `--output`.
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

Adapters share the same Actix 4.11.0 HTTP handler, header identity, worker count and
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
Dropped scheduled arrivals mark saturation; offered RPS means attempted requests
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
saturation, missing diagnostics and severe measured throttling invalidate a round.
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

CI only runs short smoke/build verification, never the full matrix or a regression
gate. Current limitations: no full rolling-window oracle, no remotely induced Redis
fault suite, no host thermal telemetry, no confidence intervals, and no trypema
adapter yet. BuildKit resource usage is not capped by Compose runtime limits.

## Timing and load diagnostics (schema 2)

Completion RPS uses at least the entire scheduled interval, plus any drain time.
It cannot exceed offered RPS due to the final pacing gap. Reports also record
completed-in-window counts, actual peak/mean in-flight requests, dispatch lag,
latency including dispatch lag, and separate 200/429/503 latency distributions.
Dropped arrivals remain explicit and invalidate the run. Concurrency is a ceiling,
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

Pull requests and manual workflow dispatch run **only short smoke**, not explore.
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
