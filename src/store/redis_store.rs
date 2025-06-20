#[cfg(feature = "redis")]
mod redis_store_impl {
    use crate::{config::RateLimitConfig, store::RateLimitStore};
    use log::{debug, error, warn};
    use redis::{Client, RedisError, RedisResult};
    use std::sync::Arc;

    const REDIS_PREFIX: &str = "rate_limit:";

    /// Implement of RateLimitStore base on redis-rs.
    pub struct RedisStore {
        client: Client,
        prefix: String,
    }

    impl RedisStore {
        /// Create a new [`crate::store::RedisStore`] instance.
        ///
        /// The URL format is `redis://[<username>][:<password>@]<hostname>[:port][/<db>]`
        pub fn new(redis_url: &str) -> Result<Self, RedisError> {
            let client = Client::open(redis_url)?;
            let mut conn = client.get_connection()?;
            let _: RedisResult<()> = redis::cmd("PING").query(&mut conn);

            Ok(Self {
                client,
                prefix: REDIS_PREFIX.to_string(),
            })
        }

        pub fn with_prefix(mut self, prefix: &str) -> Self {
            self.prefix = prefix.to_string();
            self
        }

        fn get_key(&self, key: &str) -> String {
            format!("{}{}", self.prefix, key)
        }
    }

    impl RateLimitStore for RedisStore {
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
                    // 连接失败时，不限制请求（降级策略）
                    return false;
                }
            };

            // 使用 Redis Sorted Set 存储请求时间戳
            let now = chrono::Utc::now().timestamp_millis() as f64;
            let window_start = now - config.window_secs.as_millis() as f64;

            // 1. 移除超过时间窗口的请求
            let remove_result: redis::RedisResult<i32> = redis::cmd("ZREMRANGEBYSCORE")
                .arg(&redis_key)
                .arg("-inf")
                .arg(window_start)
                .query(&mut conn);

            if let Err(err) = remove_result {
                error!("Failed to remove old entries: {}", err);
            }

            // 2. 计算当前时间窗口内的请求数量
            let count_result: redis::RedisResult<usize> = redis::cmd("ZCOUNT")
                .arg(&redis_key)
                .arg(window_start)
                .arg("+inf")
                .query(&mut conn);

            let count = match count_result {
                Ok(c) => c,
                Err(err) => {
                    error!("Redis error on ZCOUNT: {}", err);
                    // 发生错误时，不限制请求（降级策略）
                    return false;
                }
            };

            if count >= config.max_requests {
                warn!(
                    "Rate limit exceeded for key({}): count({}) >= max_req({})",
                    key, count, config.max_requests
                );
                return true;
            }

            // 3. 添加新的请求记录
            let add_result: redis::RedisResult<()> = redis::cmd("ZADD")
                .arg(&redis_key)
                .arg(now)
                .arg(now)
                .query(&mut conn);

            if let Err(err) = add_result {
                error!("Failed to add new entry: {}", err);
            }

            // 4. 设置过期时间，稍微长于窗口时间，以确保清理
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

    impl RateLimitStore for Arc<RedisStore> {
        fn is_limited(&self, key: &str, config: &RateLimitConfig) -> bool {
            (**self).is_limited(key, config)
        }
    }
}

#[cfg(feature = "redis")]
pub use redis_store_impl::RedisStore;
