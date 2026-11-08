use crate::{
    config::RateLimitConfig,
    observability::ObservedRateLimitDecision,
    store::{RateLimitDecision, RateLimitMetadata, RateLimitStore, StoreError},
};
use dashmap::DashMap;
use futures_util::future::BoxFuture;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Hash, PartialEq, Eq)]
struct BucketKey {
    namespace: String,
    client: String,
    window: Duration,
    limit: usize,
}

/// Concurrent, process-local sliding windows. Share an `Arc<MemoryStore>` across workers.
///
/// Timestamps are ordered queues: each admitted request is inserted and expired once.
/// Idle buckets are swept at most once per minute when requests arrive. Call
/// [`Self::prune_expired`] from an application timer if cleanup is needed while idle.
/// No background task is started by this store.
///
/// Detailed checks calculate the post-admission quota and earliest retry delay
/// under the same bucket lock and clock sample, without a second store lookup.
pub struct MemoryStore {
    buckets: DashMap<BucketKey, VecDeque<Instant>>,
    last_cleanup: Mutex<Instant>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
            last_cleanup: Mutex::new(Instant::now()),
        }
    }

    /// Removes inactive buckets and returns the number removed.
    pub fn prune_expired(&self) -> usize {
        self.prune_at(Instant::now())
    }

    fn prune_at(&self, now: Instant) -> usize {
        let mut removed = 0;
        self.buckets.retain(|key, timestamps| {
            let keep = timestamps
                .back()
                .is_some_and(|t| now.saturating_duration_since(*t) < key.window);
            removed += usize::from(!keep);
            keep
        });
        removed
    }

    fn cleanup_if_due(&self) {
        if let Ok(mut last) = self.last_cleanup.try_lock()
            && last.elapsed() >= Duration::from_secs(60)
        {
            self.prune_expired();
            *last = Instant::now();
        }
    }

    fn check_limited(&self, key: &str, config: &RateLimitConfig) -> Result<bool, StoreError> {
        config.validate().map_err(StoreError::new)?;
        self.cleanup_if_due();
        if config.max_requests == 0 {
            return Ok(true);
        }
        let mut timestamps = self
            .buckets
            .entry(BucketKey {
                namespace: config.namespace.clone(),
                client: key.to_owned(),
                window: config.window_secs,
                limit: config.max_requests,
            })
            .or_default();
        // Read the clock after taking the bucket lock to preserve timestamp ordering.
        Ok(Self::admit(&mut timestamps, Instant::now(), config))
    }

    fn check_detailed(
        &self,
        key: &str,
        config: &RateLimitConfig,
    ) -> Result<RateLimitDecision, StoreError> {
        config.validate().map_err(StoreError::new)?;
        self.cleanup_if_due();
        if config.max_requests == 0 {
            return Ok(RateLimitDecision {
                allowed: false,
                metadata: Some(RateLimitMetadata {
                    remaining: 0,
                    retry_after: None,
                }),
            });
        }
        let mut timestamps = self
            .buckets
            .entry(BucketKey {
                namespace: config.namespace.clone(),
                client: key.to_owned(),
                window: config.window_secs,
                limit: config.max_requests,
            })
            .or_default();
        // Admission and the snapshot must share the bucket lock and clock sample.
        Ok(Self::admit_detailed(
            &mut timestamps,
            Instant::now(),
            config,
        ))
    }

    fn admit_detailed(
        timestamps: &mut VecDeque<Instant>,
        now: Instant,
        config: &RateLimitConfig,
    ) -> RateLimitDecision {
        let limited = Self::admit(timestamps, now, config);
        RateLimitDecision {
            allowed: !limited,
            metadata: Some(RateLimitMetadata {
                remaining: config.max_requests.saturating_sub(timestamps.len()),
                retry_after: if limited {
                    timestamps.front().map(|oldest| {
                        config
                            .window_secs
                            .saturating_sub(now.saturating_duration_since(*oldest))
                    })
                } else {
                    None
                },
            }),
        }
    }

    fn admit(timestamps: &mut VecDeque<Instant>, now: Instant, config: &RateLimitConfig) -> bool {
        while timestamps
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= config.window_secs)
        {
            timestamps.pop_front();
        }
        if timestamps.len() >= config.max_requests {
            return true;
        }
        timestamps.push_back(now);
        false
    }
}

impl RateLimitStore for MemoryStore {
    fn check_observed<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
        metadata: bool,
    ) -> BoxFuture<'a, Result<ObservedRateLimitDecision, StoreError>> {
        // Reuse the synchronous admission helpers without an extra boxed future.
        // Their bucket guards are dropped before returning to the observer.
        Box::pin(async move {
            let decision = if metadata {
                self.check_detailed(key, config)?
            } else {
                RateLimitDecision {
                    allowed: !self.check_limited(key, config)?,
                    metadata: None,
                }
            };
            Ok(decision.into())
        })
    }

    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move { self.check_limited(key, config) })
    }

    fn check<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<RateLimitDecision, StoreError>> {
        Box::pin(async move { self.check_detailed(key, config) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_snapshot_uses_the_oldest_request_and_exact_expiration() {
        let config = RateLimitConfig::default().max_requests(2).window_secs(60);
        let start = Instant::now();
        let mut timestamps = VecDeque::new();
        let first = MemoryStore::admit_detailed(&mut timestamps, start, &config);
        assert!(first.allowed);
        assert_eq!(first.metadata.unwrap().remaining, 1);
        assert_eq!(first.metadata.unwrap().retry_after, None);
        let second =
            MemoryStore::admit_detailed(&mut timestamps, start + Duration::from_secs(30), &config);
        assert!(second.allowed);
        assert_eq!(second.metadata.unwrap().remaining, 0);
        let denied = MemoryStore::admit_detailed(
            &mut timestamps,
            start + Duration::from_millis(59_250),
            &config,
        );
        assert!(!denied.allowed);
        assert_eq!(denied.metadata.unwrap().remaining, 0);
        assert_eq!(
            denied.metadata.unwrap().retry_after,
            Some(Duration::from_millis(750))
        );
        assert_eq!(timestamps.len(), 2);
        let admitted =
            MemoryStore::admit_detailed(&mut timestamps, start + Duration::from_secs(60), &config);
        assert!(admitted.allowed);
        assert_eq!(admitted.metadata.unwrap().remaining, 0);
        let denied =
            MemoryStore::admit_detailed(&mut timestamps, start + Duration::from_secs(60), &config);
        assert_eq!(
            denied.metadata.unwrap().retry_after,
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn exact_window_boundary_expires_and_rejections_do_not_extend_it() {
        let config = RateLimitConfig::default().max_requests(1).window_secs(1);
        let start = Instant::now();
        let mut timestamps = VecDeque::new();
        assert!(!MemoryStore::admit(&mut timestamps, start, &config));
        assert!(MemoryStore::admit(
            &mut timestamps,
            start + Duration::from_nanos(999_999_999),
            &config
        ));
        assert_eq!(timestamps.len(), 1);
        assert!(!MemoryStore::admit(
            &mut timestamps,
            start + Duration::from_secs(1),
            &config
        ));
    }

    #[test]
    fn sweep_removes_only_expired_buckets() {
        let store = MemoryStore::new();
        let config = RateLimitConfig::default().window_secs(1);
        store.check_limited("old", &config).unwrap();
        let start = Instant::now();
        store.check_limited("active", &config).unwrap();
        for mut entry in store.buckets.iter_mut() {
            let timestamp = if entry.key().client == "old" {
                start - Duration::from_secs(1)
            } else {
                start
            };
            *entry = VecDeque::from([timestamp]);
        }
        assert_eq!(store.prune_at(start), 1);
        assert_eq!(store.buckets.len(), 1);
        assert_eq!(store.buckets.iter().next().unwrap().key().client, "active");
    }

    #[test]
    fn requests_trigger_periodic_cleanup() {
        let store = MemoryStore::new();
        let config = RateLimitConfig::default().window_secs(1);
        store.check_limited("old", &config).unwrap();
        *store.buckets.iter_mut().next().unwrap() =
            VecDeque::from([Instant::now() - Duration::from_secs(2)]);
        *store.last_cleanup.lock().unwrap() = Instant::now() - Duration::from_secs(61);
        store.check_limited("new", &config).unwrap();
        assert_eq!(store.buckets.len(), 1);
    }

    #[test]
    fn queue_matches_reference_model_over_mixed_bursts_and_expiration() {
        let config = RateLimitConfig {
            window_secs: Duration::from_nanos(100),
            max_requests: 7,
            ..Default::default()
        };
        let mut queue = VecDeque::new();
        let mut reference = Vec::new();
        let start = Instant::now();
        let mut elapsed = 0;
        let mut seed = 42_u64;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            elapsed += (seed >> 32) % 20;
            let now = start + Duration::from_nanos(elapsed);
            reference.retain(|t| now.duration_since(*t) < config.window_secs);
            let expected = reference.len() >= config.max_requests;
            if !expected {
                reference.push(now);
            }
            assert_eq!(MemoryStore::admit(&mut queue, now, &config), expected);
            assert_eq!(queue.iter().copied().collect::<Vec<_>>(), reference);
        }
    }
}
