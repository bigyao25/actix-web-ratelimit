use crate::config::RateLimitConfig;
use futures_util::future::BoxFuture;
use std::{error::Error, fmt, sync::Arc, time::Duration};

/// The result of one atomic admission, optionally including a quota snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    /// Absent when a store cannot provide a reliable snapshot (including fail-open).
    pub metadata: Option<RateLimitMetadata>,
}

/// Quota state immediately after checking and, if allowed, recording this request.
/// Concurrent requests can consume the remaining quota before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitMetadata {
    /// Available quota after admitting the current request; the `r` header parameter.
    /// This snapshot does not reserve quota for future requests.
    pub remaining: usize,
    /// Earliest retry delay for a rejected request, assuming no competing admissions.
    /// `None` for admitted requests, unknown delays, or a permanently zero quota.
    pub retry_after: Option<Duration>,
}

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

    /// Checks and records exactly once, with optional metadata from the same operation.
    /// The default calls [`Self::is_limited`] exactly once and returns
    /// `metadata: None`, preserving existing custom stores without inventing quota
    /// information. Override this method to provide a snapshot atomically with
    /// admission. `Arc` and `Box` forward both methods to the underlying store.
    ///
    /// Direct callers can use `store.check(key, &config).await?` to obtain a
    /// [`RateLimitDecision`] with optional [`RateLimitMetadata`]. Call this OR
    /// `is_limited` for a request; calling both performs admission twice.
    fn check<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<RateLimitDecision, StoreError>> {
        Box::pin(async move {
            Ok(RateLimitDecision {
                allowed: !self.is_limited(key, config).await?,
                metadata: None,
            })
        })
    }
}

impl<T: RateLimitStore + ?Sized> RateLimitStore for Box<T> {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        (**self).is_limited(key, config)
    }

    fn check<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<RateLimitDecision, StoreError>> {
        (**self).check(key, config)
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

    fn check<'a>(
        &'a self,
        key: &'a str,
        config: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<RateLimitDecision, StoreError>> {
        (**self).check(key, config)
    }
}
