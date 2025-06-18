use crate::config::RateLimitConfig;

pub trait RateLimitStore: Send + Sync {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool;
}

// Implement the trait for a trait object of itself, to support dynamic dispatch
impl RateLimitStore for Box<dyn RateLimitStore> {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}

// Implement the trait for Arc wrapped trait object
impl RateLimitStore for std::sync::Arc<dyn RateLimitStore> {
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}
