#[cfg(feature = "redis")]
mod redis_store_impl {
    use crate::{config::RateLimitConfig, store::RateLimitStore};
    use log::{debug, error, warn};
    use redis::{Client, RedisError, RedisResult};
    use std::sync::Arc;

    /// Default prefix for Redis keys used by the rate limiter
    const REDIS_PREFIX: &str = "rate_limit:";

    /// Redis-based implementation of [`RateLimitStore`] using Redis Sorted Sets.
    ///
    /// This store uses Redis Sorted Sets to track request timestamps for each client.
    /// It's suitable for distributed applications where rate limiting data needs
    /// to be shared across multiple instances.
    ///
    /// # Features
    ///
    /// - **Distributed**: Share rate limiting data across multiple application instances
    /// - **Persistent**: Data survives application restarts
    /// - **Scalable**: Can handle high throughput with proper Redis configuration
    /// - **Automatic cleanup**: Uses Redis expiration to clean up old data
    ///
    /// # Redis Data Structure
    ///
    /// Uses Redis Sorted Sets where:
    /// - Key: `{prefix}{client_id}`
    /// - Score: Request timestamp in milliseconds
    /// - Member: Same as score (timestamp)
    ///
    /// # Fallback Strategy
    ///
    /// If Redis operations fail, the store falls back to allowing requests
    /// to prevent service disruption.
    pub struct RedisStore {
        /// Redis client for database operations
        client: Client,
        /// Key prefix for namespacing rate limit data
        prefix: String,
    }

    impl RedisStore {
        /// Creates a new [`RedisStore`] instance and tests the connection.
        ///
        /// # Arguments
        ///
        /// * `redis_url` - Redis connection URL
        ///
        /// # URL Format
        ///
        /// `redis://[<username>][:<password>@]<hostname>[:port][/<db>]`
        ///
        /// # Examples
        ///
        /// ```rust,no_run
        /// # #[cfg(feature = "redis")]
        /// # {
        /// use actix_web_ratelimit::store::RedisStore;
        ///
        /// // Basic connection
        /// let store = RedisStore::new("redis://127.0.0.1/")?;
        ///
        /// // With authentication and specific database
        /// let store = RedisStore::new("redis://:password@127.0.0.1:6379/1")?;
        /// # }
        /// # Ok::<(), redis::RedisError>(())
        /// ```
        ///
        /// # Errors
        ///
        /// Returns [`RedisError`] if:
        /// - URL format is invalid
        /// - Cannot connect to Redis server
        /// - PING command fails
        pub fn new(redis_url: &str) -> Result<Self, RedisError> {
            let client = Client::open(redis_url)?;
            let mut conn = client.get_connection()?;
            let _: RedisResult<()> = redis::cmd("PING").query(&mut conn);

            Ok(Self {
                client,
                prefix: REDIS_PREFIX.to_string(),
            })
        }

        /// Sets a custom prefix for Redis keys.
        ///
        /// This is useful for namespacing when multiple applications
        /// or environments share the same Redis instance.
        ///
        /// # Arguments
        ///
        /// * `prefix` - Custom prefix for Redis keys
        ///
        /// # Example
        ///
        /// ```rust,no_run
        /// # #[cfg(feature = "redis")]
        /// # {
        /// use actix_web_ratelimit::store::RedisStore;
        ///
        /// let store = RedisStore::new("redis://127.0.0.1/")?
        ///     .with_prefix("myapp:ratelimit:");
        /// # }
        /// # Ok::<(), redis::RedisError>(())
        /// ```
        pub fn with_prefix(mut self, prefix: &str) -> Self {
            self.prefix = prefix.to_string();
            self
        }

        /// Generates the full Redis key by combining prefix and client identifier.
        ///
        /// # Arguments
        ///
        /// * `key` - Client identifier (typically IP address)
        ///
        /// # Returns
        ///
        /// Full Redis key string
        fn get_key(&self, key: &str) -> String {
            format!("{}{}", self.prefix, key)
        }
    }

    impl RateLimitStore for RedisStore {
        /// Checks if the client has exceeded the rate limit using Redis Sorted Sets.
        ///
        /// This method implements a distributed sliding window algorithm:
        /// 1. Removes expired request timestamps from the sorted set
        /// 2. Counts remaining requests in the time window
        /// 3. Checks if count exceeds the configured limit
        /// 4. If not exceeded, adds current timestamp to the set
        /// 5. Sets expiration time for automatic cleanup
        ///
        /// # Fallback Strategy
        ///
        /// If any Redis operation fails, the method returns `false` (allow request)
        /// to prevent service disruption. Errors are logged for monitoring.
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
            use std::i32;

            let redis_key = self.get_key(key);

            debug!(
                "Checking rate limit for key: {} with config: max_req={}, window={:?}",
                key, config.max_requests, config.window_secs
            );

            let mut conn = match self.client.get_connection() {
                Ok(conn) => conn,
                Err(err) => {
                    error!("Failed to get Redis connection: {}", err);
                    // Fallback: allow request when connection fails (graceful degradation)
                    return false;
                }
            };

            // Use Redis Sorted Set to store request timestamps
            let now = chrono::Utc::now().timestamp_millis() as f64;
            let window_start = now - config.window_secs.as_millis() as f64;

            // Step 1: Remove expired requests outside the time window
            let remove_result: redis::RedisResult<i32> = redis::cmd("ZREMRANGEBYSCORE")
                .arg(&redis_key)
                .arg("-inf")
                .arg(window_start)
                .query(&mut conn);

            if let Err(err) = remove_result {
                error!("Failed to remove old entries: {}", err);
            }

            // Step 2: Count current requests within the time window
            let count_result: redis::RedisResult<usize> = redis::cmd("ZCOUNT")
                .arg(&redis_key)
                .arg(window_start)
                .arg("+inf")
                .query(&mut conn);

            let count = match count_result {
                Ok(c) => c,
                Err(err) => {
                    error!("Redis error on ZCOUNT: {}", err);
                    // Fallback: allow request when count fails (graceful degradation)
                    return false;
                }
            };

            if count > config.max_requests {
                warn!(
                    "Rate limit exceeded for key({}): count({}) >= max_req({})",
                    key, count, config.max_requests
                );
                return true;
            }

            // Step 3: Add current request timestamp
            let add_result: redis::RedisResult<()> = redis::cmd("ZADD")
                .arg(&redis_key)
                .arg(now)
                .arg(now)
                .query(&mut conn);

            if let Err(err) = add_result {
                error!("Failed to add new entry: {}", err);
            }

            // Step 4: Set expiration time slightly longer than window for cleanup
            let expiry = config.window_secs.as_secs() + 10;
            let expire_result: redis::RedisResult<()> = redis::cmd("EXPIRE")
                .arg(&redis_key)
                .arg(expiry as i64)
                .query(&mut conn);

            if let Err(err) = expire_result {
                error!("Failed to set expiry: {}", err);
            }

            false
        }
    }

    /// Implementation of [`RateLimitStore`] for `Arc<RedisStore>` to enable shared ownership.
    ///
    /// This allows the same `RedisStore` instance to be used across multiple threads
    /// and middleware instances safely.
    impl RateLimitStore for Arc<RedisStore> {
        /// Delegates to the underlying `RedisStore` implementation.
        fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
            (**self).is_limited(key, config)
        }
    }
}

#[cfg(feature = "redis")]
pub use redis_store_impl::RedisStore;
