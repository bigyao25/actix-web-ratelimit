# Competitor integration evidence

Verified 2026-11-05 using official sparse-index records and published source archives.
All dependencies are confined to Benchmark adapters; production dependencies are unchanged.
Every adapter now pins **actix-web =4.18.1**, including baseline/history/governor.
Older 4.11.0 results must be rebaselined; do not attribute a common HTTP-stack change
to a limiter optimization. Each adapter has an independent exact Cargo lockfile.

| Adapter | Published version | Backend | Policy | Comparison boundary |
| --- | --- | --- | --- | --- |
| head / v0.2.1 | local SHA / =0.2.1 | Memory, Redis | Exact admitted-request sliding log | Strict atomic admission |
| governor | =0.10.0 | Memory | GCRA, continuous refill and burst | No zero-quota equivalent; not strict rolling-log semantics |
| trypema | actix-trypema =0.1.2; trypema =2.2.1 | Local, Redis | Bucketed sliding counters | Timestamp coalescing; Local admission documented best-effort under concurrency |
| extensible | actix-extensible-rate-limit =0.4.0 | Memory, Redis | First-request-anchored fixed window | New full quota at reset; arbitrary rolling-window bursts differ |

The previous trypema exclusion was caused by the harness's own Actix 4.11 pin.
Upgrading *every* adapter resolves its >=4.15 dependency requirement fairly.
Trypema uses its published Local/Redis defaults and absolute strategy. Its rate is
quota/window; zero is unsupported and deny cells are skipped explicitly. Local
`inc` documents non-atomic check/increment across concurrent calls, timestamp
coalescing, and that rejected increments are not recorded. No external locking is
added to make it appear equivalent. Redis policy is observed independently.

Extensible uses `SimpleInputFunctionBuilder::custom_fn` with the same header key,
and its published InMemoryBackend/RedisBackend. Memory request() counts attempts
until an anchored TTL reset. Redis uses its own implementation unchanged.
The adapter enables its Redis feature and pins its compatible redis =0.26.1.
Published 0.4.0 emits a Rust future-incompatibility warning; compilation currently
succeeds. Do not suppress this warning or silently patch the crate.

All adapters share the handler body `ok`, configured worker count, identity header,
Docker quotas and request trace. Library-default rejection bodies/headers differ;
these are end-to-end product costs, not isolated algorithm microbenchmarks.
No algorithm or failure policy is rewritten for a better score.

## High-load comparison

The user-facing comparison is now the manual `compare` suite, with the native generator
and identical resource limits for each variant. Low-load behavior probes have been
removed. Assertions use the shared long first-window oracle where applicable;
GCRA and best-effort bucketed admission remain explicitly non-equivalent. Their
accepted counts and nominal excess must be shown, not patched into equivalence.

At least five valid repetitions, >=99% offered-rate completion, scheduled p99
<=10 ms and no generator CPU pressure are required to qualify a tested rate.
Reported levels and cost ratios are scoped to the actual host/resources/workload.
No cross-device ranking or automatic fastest-crate claim is made. Low-level
source guarantees in the table above remain context for interpreting the results.

Sources (published code was read, not inferred from crate names):
- https://index.crates.io/ac/ti/actix-governor
- https://index.crates.io/ac/ti/actix-trypema
- https://index.crates.io/tr/yp/trypema
- https://index.crates.io/ac/ti/actix-extensible-rate-limit
- https://static.crates.io/crates/actix-trypema/actix-trypema-0.1.2.crate
- https://static.crates.io/crates/trypema/trypema-2.2.1.crate
- https://static.crates.io/crates/actix-extensible-rate-limit/actix-extensible-rate-limit-0.4.0.crate
