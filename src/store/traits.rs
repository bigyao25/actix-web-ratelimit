use crate::config::RateLimitConfig;
use futures_util::future::BoxFuture;
use std::{error::Error, fmt, sync::Arc};

/// An invalid configuration or an unavailable storage backend.
#[derive(Debug)]
pub struct StoreError(Box<dyn Error + Send + Sync>);

impl StoreError {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Checks and records requests in the half-open window `(now - window, now]`.
///
/// Only admitted requests are recorded. `Ok(true)` rejects a request with HTTP 429;
/// an error produces HTTP 503 in the middleware. Implementations must perform the
/// check and admission atomically, and must not block the executor with network I/O.
///
/// ```
/// use actix_web_ratelimit::{config::RateLimitConfig, store::{RateLimitStore, StoreError}};
/// use futures_util::future::BoxFuture;
///
/// struct DenyAll;
/// impl RateLimitStore for DenyAll {
///     fn is_limited<'a>(&'a self, _key: &'a str, config: &'a RateLimitConfig)
///         -> BoxFuture<'a, Result<bool, StoreError>>
///     {
///         Box::pin(async move {
///             config.validate().map_err(StoreError::new)?;
///             Ok(true)
///         })
///     }
/// }
/// ```
pub trait RateLimitStore: Send + Sync {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
}

impl<T: RateLimitStore + ?Sized> RateLimitStore for Box<T> {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        (**self).is_limited(key, config)
    }
}

impl<T: RateLimitStore + ?Sized> RateLimitStore for Arc<T> {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        (**self).is_limited(key, config)
    }
}
