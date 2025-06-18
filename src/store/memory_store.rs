use dashmap::DashMap;
use std::{sync::Arc, time::Instant};

use crate::{config::RateLimitConfig, store::RateLimitStore};

/// Implement of RateLimitStore base on memory
pub struct MemoryStore {
    pub store: DashMap<String, Vec<Instant>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

// Implement RateLimitStore for Arc<MemoryStore>
impl RateLimitStore for Arc<MemoryStore> {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        let now = Instant::now();
        let mut entry = self.store.entry(key.to_string()).or_default();
        let timestamps = entry.value_mut();

        // 保留窗口内的请求时间
        timestamps.retain(|&t| now.duration_since(t) <= config.window_secs);
        if timestamps.len() >= config.max_requests {
            // error!(
            //     "ts:[{:?}], len:{}, max_r:{}, w:{:?}",
            //     timestamps
            //         .iter()
            //         .map(|i| i.elapsed().as_secs_f32())
            //         .collect::<Vec<f32>>(),
            //     timestamps.len(),
            //     config.max_requests,
            //     config.window_secs
            // );
            return true;
        }

        timestamps.push(now);
        false
    }
}

/// Implement of RateLimitStore
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
