use dashmap::DashMap;
use std::{sync::Arc, time::Instant};

use crate::{config::RateLimitConfig, store::RateLimitStore};

/// In-memory implementation of [`RateLimitStore`] using DashMap for concurrent access.
///
/// This store uses a thread-safe HashMap (DashMap) to store request timestamps
/// for each client identifier. It's suitable for single-instance applications
/// where rate limiting data doesn't need to be shared across multiple processes.
///
/// # Performance
///
/// - Fast access with O(1) lookup time
/// - Thread-safe concurrent operations
/// - Memory usage grows with the number of unique clients
///
/// # Limitations
///
/// - Data is lost on application restart
/// - Not suitable for distributed systems
/// - Memory usage can grow if clients are not cleaned up
pub struct MemoryStore {
    /// Thread-safe map storing client identifiers and their request timestamps
    pub store: DashMap<String, Vec<Instant>>,
}

impl MemoryStore {
    /// Creates a new [`MemoryStore`] instance with an empty DashMap.
    ///
    /// # Returns
    ///
    /// A new `MemoryStore` instance ready for use.
    ///
    /// # Example
    ///
    /// ```rust
    /// use actix_web_ratelimit::store::MemoryStore;
    /// use std::sync::Arc;
    ///
    /// let store = Arc::new(MemoryStore::new());
    /// ```
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }
}

/// Default implementation that creates a new [`MemoryStore`] instance.
///
/// This is equivalent to calling [`MemoryStore::new()`].
impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitStore for MemoryStore {
    /// Checks if the client has exceeded the rate limit and records the current request.
    ///
    /// This method implements the sliding window algorithm:
    /// 1. Gets or creates an entry for the client key
    /// 2. Removes expired timestamps outside the time window
    /// 3. Checks if the remaining request count exceeds the limit
    /// 4. If not exceeded, records the current timestamp
    ///
    /// # Arguments
    ///
    /// * `key` - Client identifier (typically IP address)
    /// * `config` - Rate limiting configuration
    ///
    /// # Returns
    ///
    /// `true` if the client has exceeded the rate limit, `false` otherwise
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        let now = Instant::now();
        let mut entry = self.store.entry(key.to_string()).or_default();
        let timestamps = entry.value_mut();

        // Keep only timestamps within the time window
        timestamps.retain(|&t| now.duration_since(t) <= config.window_secs);
        if timestamps.len() > config.max_requests {
            return true;
        }

        timestamps.push(now);
        false
    }
}

/// Implementation of [`RateLimitStore`] for `Arc<MemoryStore>` to enable shared ownership.
///
/// This allows the same `MemoryStore` instance to be used across multiple threads
/// and middleware instances safely.
impl RateLimitStore for Arc<MemoryStore> {
    /// Delegates to the underlying `MemoryStore` implementation.
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}
