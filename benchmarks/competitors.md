# Competitor integration evidence

Checked against official crates.io sparse index and published crate archives on
2026-11-05. These are benchmark-only dependencies, never production dependencies.

- actix-governor **0.10.0**: governor 0.10.1 GCRA, configurable period and burst,
  custom `KeyExtractor`. Memory adapter included as `--variants governor`.
  GCRA refills continuously and is not equivalent to exact sliding-window logs.
  Zero quota has no equivalent; deny and Redis cells are explicitly skipped.
  The strict-window over-admission oracle must not be applied to GCRA.
- actix-trypema **0.1.2**: Local, Redis and Hybrid, with the `redis` feature forwarding
  to trypema/redis-tokio. Published manifest requires actix-web **4.15**, trypema 2.2,
  Rust >=1.88. This harness pins actix-web **4.11.0** across baseline and historical
  adapters; Cargo cannot unify these constraints. Not integrated in this revision.
  Next integration should update *every* adapter's common HTTP dependency and lock
  files together, then independently verify bucket resolution/expiration semantics.
  No competitor performance conclusions can be drawn from an unavailable adapter.

Sources: https://index.crates.io/ac/ti/actix-governor and
https://index.crates.io/ac/ti/actix-trypema; source archives under
https://static.crates.io/crates/actix-governor/ and
https://static.crates.io/crates/actix-trypema/.
