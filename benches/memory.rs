//! Run with `cargo bench --bench memory --no-default-features`.
use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{MemoryStore, RateLimitStore},
};
use dashmap::DashMap;
use futures_util::FutureExt;
use std::{hint::black_box, time::Instant};

fn main() {
    if cfg!(debug_assertions) {
        return;
    }
    const REQUESTS: usize = 200_000;
    let config = RateLimitConfig::default().max_requests(1_000);
    for clients in [1, 100, 10_000] {
        let keys: Vec<_> = (0..clients).map(|i| format!("client-{i}")).collect();
        let old: DashMap<String, Vec<Instant>> = DashMap::new();
        let start = Instant::now();
        let mut old_allowed = 0;
        for i in 0..REQUESTS {
            let mut timestamps = old.entry(keys[i % clients].clone()).or_default();
            let now = Instant::now();
            timestamps.retain(|t| now.duration_since(*t) < config.window_secs);
            if timestamps.len() < config.max_requests {
                timestamps.push(now);
                old_allowed += 1;
            }
            black_box(&timestamps);
        }
        let old_elapsed = start.elapsed();
        let store = MemoryStore::new();
        let start = Instant::now();
        let mut allowed = 0;
        for i in 0..REQUESTS {
            allowed += usize::from(!black_box(
                store
                    .is_limited(&keys[i % clients], &config)
                    .now_or_never()
                    .unwrap()
                    .unwrap(),
            ));
        }
        let elapsed = start.elapsed();
        assert_eq!(allowed, old_allowed);
        let detailed_store = MemoryStore::new();
        let start = Instant::now();
        let mut detailed_allowed = 0;
        for i in 0..REQUESTS {
            let decision = black_box(
                detailed_store
                    .check(&keys[i % clients], &config)
                    .now_or_never()
                    .unwrap()
                    .unwrap(),
            );
            detailed_allowed += usize::from(decision.allowed);
        }
        let detailed_elapsed = start.elapsed();
        assert_eq!(detailed_allowed, allowed);
        println!(
            "clients={clients} requests={REQUESTS} admitted={allowed} vec_retain_ns_per_check={:.0} queue_ns_per_check={:.0} detailed_ns_per_check={:.0}",
            old_elapsed.as_nanos() as f64 / REQUESTS as f64,
            elapsed.as_nanos() as f64 / REQUESTS as f64,
            detailed_elapsed.as_nanos() as f64 / REQUESTS as f64
        );
    }
}
