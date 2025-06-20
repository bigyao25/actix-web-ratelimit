use dashmap::DashMap;
use std::{sync::Arc, time::Instant};

use crate::{config::RateLimitConfig, store::RateLimitStore};

/// Implement of [`crate::store::RateLimitStore`] base on dashmap.
pub struct MemoryStore {
    pub store: DashMap<String, Vec<Instant>>,
}

impl MemoryStore {
    /*!
    Create a new [`crate::store::MemoryStore`] instance.

    Example:
    ```rust
    use actix_web_ratelimit::store::MemoryStore;

    let store = Arc::new(MemoryStore::new());
    ```
    */
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }
}

/// Equivalent to MemoryStore::new() method.
impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitStore for MemoryStore {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        let now = Instant::now();
        let mut entry = self.store.entry(key.to_string()).or_default();
        let timestamps = entry.value_mut();

        // 保留窗口内的请求时间
        timestamps.retain(|&t| now.duration_since(t) <= config.window_secs);
        if timestamps.len() >= config.max_requests {
            return true;
        }

        timestamps.push(now);
        false
    }
}

impl RateLimitStore for Arc<MemoryStore> {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}
