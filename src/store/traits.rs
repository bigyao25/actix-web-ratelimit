use crate::config::RateLimitConfig;

/// Trait defining the storage interface for rate limiting data.
///
/// This trait abstracts the storage mechanism used to track request timestamps
/// and determine if clients have exceeded their rate limits. Implementations
/// can use various backends like in-memory storage, Redis, databases, etc.
///
/// # Thread Safety
///
/// All implementations must be `Send + Sync` to support concurrent access
/// across multiple threads in the actix-web runtime.
///
/// # Implementations
///
/// The crate provides two built-in implementations:
/// - [`crate::store::MemoryStore`]: In-memory storage using DashMap
/// - [`crate::store::RedisStore`]: Distributed storage using Redis (requires `redis` feature)
///
/// # Custom Implementations
///
/// You can create custom storage backends by implementing this trait:
///
/// ```rust
/// use actix_web_ratelimit::{store::RateLimitStore, config::RateLimitConfig};
///
/// struct CustomStore {
///     // Your storage implementation
/// }
///
/// impl RateLimitStore for CustomStore {
///     fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
///         // Your rate limiting logic here
///         // Return true if client has exceeded the limit
///         false
///     }
/// }
/// ```
pub trait RateLimitStore: Send + Sync {
    /// Checks if a client has exceeded the rate limit and records the current request.
    ///
    /// # Arguments
    ///
    /// * `key` - Client identifier (typically IP address, but can be customized)
    /// * `config` - Rate limiting configuration containing limits and time window
    ///
    /// # Returns
    ///
    /// * `true` - Client has exceeded the rate limit (request should be rejected)
    /// * `false` - Client is within limits (request should be allowed)
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool;
}

/// Implementation of [`RateLimitStore`] for `Box<dyn RateLimitStore>` to support dynamic dispatch.
///
/// This allows using different store implementations behind a trait object,
/// enabling runtime selection of storage backends.
///
/// # Example
///
/// ```rust
/// use actix_web_ratelimit::store::{RateLimitStore, MemoryStore};
///
/// let store: Box<dyn RateLimitStore> = Box::new(MemoryStore::new());
/// // Now you can use `store` as a trait object
/// ```
impl RateLimitStore for Box<dyn RateLimitStore> {
    /// Delegates to the underlying implementation.
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}

/// Implementation of [`RateLimitStore`] for `Arc<dyn RateLimitStore>` to support shared ownership.
///
/// This allows sharing the same store implementation across multiple threads
/// and middleware instances using atomic reference counting.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use actix_web_ratelimit::store::{RateLimitStore, MemoryStore};
///
/// let store: Arc<dyn RateLimitStore> = Arc::new(MemoryStore::new());
/// let store_clone = store.clone();
/// // Both `store` and `store_clone` reference the same underlying implementation
/// ```
impl RateLimitStore for std::sync::Arc<dyn RateLimitStore> {
    /// Delegates to the underlying implementation.
    fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
        (**self).is_limited(key, config)
    }
}
