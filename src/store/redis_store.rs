use crate::{
    config::RateLimitConfig,
    store::{RateLimitStore, StoreError},
};
use futures_util::future::BoxFuture;
use redis::{
    Client, RedisError, RedisResult, Script,
    aio::{ConnectionManager, ConnectionManagerConfig},
};
use std::{io, sync::LazyLock, time::Duration};

static SCRIPT: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("sliding_window.lua")));

/// Runtime Redis failure handling. Startup connection failures always return an error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FailurePolicy {
    /// Preserve availability by admitting requests when Redis is unavailable.
    #[default]
    FailOpen,
    /// Return an error; the middleware responds with HTTP 503.
    FailClosed,
}

#[derive(Debug, Clone)]
pub struct RedisStoreOptions {
    /// Total deadline for initialization or a check, including reconnect waits.
    pub timeout: Duration,
    pub failure_policy: FailurePolicy,
}

impl Default for RedisStoreOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
            failure_policy: FailurePolicy::FailOpen,
        }
    }
}

/// Atomic distributed sliding windows using server time and a shared async connection.
///
/// Clones multiplex the same connection and reconnect automatically. Initialize the
/// store once before starting workers. Keys encode prefix, rule, window, quota and
/// client ID. Client IDs should be internal identifiers, never raw credentials.
#[derive(Clone)]
pub struct RedisStore {
    connection: ConnectionManager,
    prefix: String,
    options: RedisStoreOptions,
}

impl RedisStore {
    /// Connects and verifies Redis asynchronously, using the default options.
    pub async fn new(redis_url: &str) -> RedisResult<Self> {
        Self::with_options(redis_url, RedisStoreOptions::default()).await
    }

    /// Connects, verifies PING and loads the admission script within one deadline.
    pub async fn with_options(redis_url: &str, options: RedisStoreOptions) -> RedisResult<Self> {
        if options.timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Redis timeout must be greater than zero",
            )
            .into());
        }
        let client = Client::open(redis_url)?;
        let config = ConnectionManagerConfig::new()
            .set_connection_timeout(Some(options.timeout))
            .set_response_timeout(Some(options.timeout));
        let connection = tokio::time::timeout(options.timeout, async {
            let mut connection = ConnectionManager::new_with_config(client, config).await?;
            redis::cmd("PING")
                .query_async::<String>(&mut connection)
                .await?;
            SCRIPT.load_async(&mut connection).await?;
            Ok::<_, RedisError>(connection)
        })
        .await
        .map_err(|_| Self::timeout_error())??;
        Ok(Self {
            connection,
            prefix: "rate_limit:".to_owned(),
            options,
        })
    }

    pub fn with_prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_owned();
        self
    }

    fn timeout_error() -> RedisError {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Redis rate limit deadline exceeded",
        )
        .into()
    }

    fn get_key(&self, key: &str, config: &RateLimitConfig) -> String {
        // Byte-length framing prevents collisions even when identifiers contain colons.
        format!(
            "{}v2:{}:{}:{}:{}:{}:{}",
            self.prefix,
            config.namespace.len(),
            config.namespace,
            config.window_secs.as_nanos(),
            config.max_requests,
            key.len(),
            key
        )
    }

    async fn check(&self, key: &str, config: &RateLimitConfig) -> RedisResult<bool> {
        let mut connection = self.connection.clone();
        // Round up sub-microsecond durations so Redis never expires a request early.
        let window_micros = config.window_secs.as_nanos().div_ceil(1_000) as u64;
        let limited: bool = SCRIPT
            .key(self.get_key(key, config))
            .arg(window_micros)
            .arg(config.max_requests)
            .invoke_async(&mut connection)
            .await?;
        Ok(limited)
    }
}

impl RateLimitStore for RedisStore {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            config.validate().map_err(StoreError::new)?;
            if config.max_requests == 0 {
                return Ok(true);
            }
            let result = tokio::time::timeout(self.options.timeout, self.check(key, config))
                .await
                .unwrap_or_else(|_| Err(Self::timeout_error()));
            match result {
                Ok(limited) => Ok(limited),
                Err(error) => {
                    // Do not log client IDs, Redis URLs or server-provided error details.
                    log::warn!(
                        "Redis rate limit check failed; policy={:?}",
                        self.options.failure_policy
                    );
                    match self.options.failure_policy {
                        FailurePolicy::FailOpen => Ok(false),
                        FailurePolicy::FailClosed => Err(StoreError::new(error)),
                    }
                }
            }
        })
    }
}
