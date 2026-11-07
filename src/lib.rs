//! Sliding-window rate limiting middleware for Actix Web 4.
//!
//! The memory backend is process-local. With the default `redis` feature, the Redis
//! backend shares atomic quotas across workers and application instances.
//!
//! ```rust,no_run
//! use actix_web::{App, HttpServer, HttpResponse, web};
//! use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
//! use std::sync::Arc;
//!
//! #[actix_web::main]
//! async fn main() -> std::io::Result<()> {
//!     let store = Arc::new(MemoryStore::new());
//!     let config = RateLimitConfig::default().namespace("api").max_requests(100).window_secs(60);
//!     HttpServer::new(move || App::new()
//!         .wrap(RateLimit::new(config.clone(), store.clone()))
//!         .route("/", web::get().to(|| async { HttpResponse::Ok().body("Hello") })))
//!         .bind(("127.0.0.1", 8080))?.run().await
//! }
//! ```
//!
//! Redis initialization and direct checks are asynchronous:
//!
//! ```rust,no_run
//! # #[cfg(feature = "redis")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use actix_web_ratelimit::{config::RateLimitConfig, store::{RedisStore, RateLimitStore}};
//! let store = RedisStore::new("redis://127.0.0.1:6379/").await?;
//! let limited = store.is_limited("internal-user-id", &RateLimitConfig::default()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The window is `(now - window, now]`; rejected requests do not extend it. A zero
//! quota denies all requests. Zero windows and out-of-range numeric settings are
//! rejected by [`config::RateLimitConfig::validate`] and [`RateLimit::try_new`].
//! The default identity is the peer IP, without its source port. Applications behind
//! trusted proxies should supply a validated identity extractor via
//! [`config::RateLimitConfig::id`]. Backend errors become HTTP 503; exhausted quotas
//! invoke the configurable HTTP 429 callback. See the repository README for options,
//! failure policies, testing commands and custom store implementations.

pub mod config;
mod headers;
pub mod observability;
pub mod store;

// Compile both READMEs as doctests so their public API examples cannot silently drift.
#[cfg(doctest)]
mod readme_examples {
    #[doc = include_str!("../README.md")]
    struct English;

    #[doc = include_str!("../README-cn.md")]
    struct Chinese;
}

use actix_service::{Service, Transform};
use actix_web::{
    Error,
    body::EitherBody,
    dev::{ServiceRequest, ServiceResponse},
};
use futures_util::future::{LocalBoxFuture, Ready, ok};
use std::{
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

use crate::{
    config::{ConfigError, RateLimitConfig},
    headers::ResponseHeaders,
    observability::RateLimitObserver,
    store::{RateLimitDecision, RateLimitStore},
};

pub struct RateLimit<S>
where
    S: RateLimitStore,
{
    store: Arc<S>,
    config: Arc<RateLimitConfig>,
    response_headers: bool,
    observer: Option<Arc<dyn RateLimitObserver>>,
}

impl<S> RateLimit<S>
where
    S: RateLimitStore,
{
    /// Creates middleware. Panics for an invalid window or numeric range.
    /// Use [`Self::try_new`] to handle configuration errors without panicking.
    #[track_caller]
    pub fn new(config: RateLimitConfig, store: S) -> Self {
        Self::try_new(config, store).expect("invalid rate limit configuration")
    }

    pub fn try_new(config: RateLimitConfig, store: S) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            store: Arc::new(store),
            config: Arc::new(config),
            response_headers: false,
            observer: None,
        })
    }

    /// Opts into quota response headers (disabled by default).
    ///
    /// Disabled middleware calls only [`RateLimitStore::is_limited`], without
    /// metadata lookup or header formatting. Enabled middleware calls only
    /// [`RateLimitStore::check`], exactly once per request. Missing metadata (including
    /// legacy stores and Redis fail-open) omits all automatic quota response fields.
    ///
    /// Reliable metadata produces `RateLimit-Policy` and `RateLimit` on admitted and
    /// rejected responses, following [draft-ietf-httpapi-ratelimit-headers-11], which
    /// is not a finalized RFC. The optional `t` parameter is omitted. The policy name
    /// uses the namespace with quotes and backslashes escaped. Non-ASCII/control
    /// characters or quotas above the Structured Fields integer maximum
    /// (`999999999999999`) omit both draft fields, without changing admission or
    /// `Retry-After`. Fractional-second windows omit the optional `w` parameter.
    ///
    /// Our HTTP 429 rejections include `Retry-After` when a delay is known, rounded
    /// up to seconds. Built-in stores use the oldest active request's expiration,
    /// rather than the full window. A zero quota has no finite recovery time and
    /// omits this field. Other callback statuses and downstream HTTP 429 responses
    /// do not receive an automatic `Retry-After`. Existing response fields are
    /// preserved individually, and downstream service errors propagate unchanged.
    ///
    /// [draft-ietf-httpapi-ratelimit-headers-11]: https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-ratelimit-headers-11
    pub fn response_headers(mut self, enabled: bool) -> Self {
        self.response_headers = enabled;
        self
    }

    /// Registers a shared, synchronous observer; independent of response headers.
    /// No extra clock samples or events are created unless registered. Checks
    /// still execute exactly once. See [`RateLimitObserver`] for callback safety,
    /// non-blocking requirements and panic isolation limits.
    ///
    /// ```
    /// use actix_web_ratelimit::{RateLimit, config::RateLimitConfig,
    ///     store::MemoryStore, observability::RateLimitEvent};
    /// use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
    /// let count = Arc::new(AtomicU64::new(0));
    /// let events = count.clone();
    /// let middleware = RateLimit::new(RateLimitConfig::default(), MemoryStore::new())
    ///     .observe(move |_: &RateLimitEvent<'_>| {
    ///         events.fetch_add(1, Ordering::Relaxed);
    ///     });
    /// ```
    pub fn observe(mut self, observer: impl RateLimitObserver + 'static) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }
}

impl<S, B, ST> Transform<S, ServiceRequest> for RateLimit<ST>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
    ST: RateLimitStore + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Transform = RateLimitMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(RateLimitMiddleware {
            service: Rc::new(service),
            store: match &self.observer {
                Some(observer) => Arc::new(observability::ObservedStore {
                    store: self.store.clone(),
                    observer: observer.clone(),
                }),
                None => self.store.clone(),
            },
            config: self.config.clone(),
            response_headers: self
                .response_headers
                .then(|| Arc::new(ResponseHeaders::new(&self.config))),
        })
    }
}

pub struct RateLimitMiddleware<S> {
    service: Rc<S>,
    store: Arc<dyn RateLimitStore>,
    config: Arc<RateLimitConfig>,
    response_headers: Option<Arc<ResponseHeaders>>,
}

impl<S, B> Service<ServiceRequest> for RateLimitMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let store = self.store.clone();
        let config = self.config.clone();
        let service = self.service.clone();
        let response_headers = self.response_headers.clone();
        Box::pin(async move {
            let id = (config.get_id)(&req);
            let result = if response_headers.is_some() {
                store.check(&id, &config).await
            } else {
                store
                    .is_limited(&id, &config)
                    .await
                    .map(|limited| RateLimitDecision {
                        allowed: !limited,
                        metadata: None,
                    })
            };
            let metadata = match result {
                Ok(decision) if !decision.allowed => {
                    let mut response = (config.on_exceed)(&id, &config, &req);
                    if let Some(headers) = &response_headers
                        && let Some(metadata) = decision.metadata
                    {
                        let rejected_with_429 =
                            response.status() == actix_web::http::StatusCode::TOO_MANY_REQUESTS;
                        headers.apply(response.headers_mut(), metadata, rejected_with_429);
                    }
                    return Ok(req.into_response(response).map_into_right_body());
                }
                Err(_) => {
                    return Ok(req
                        .into_response(
                            actix_web::HttpResponse::ServiceUnavailable()
                                .body("Rate limit storage unavailable"),
                        )
                        .map_into_right_body());
                }
                Ok(decision) => decision.metadata,
            };
            let mut res = service.call(req).await?;
            if let Some(headers) = &response_headers
                && let Some(metadata) = metadata
            {
                headers.apply(res.headers_mut(), metadata, false);
            }
            Ok(res.map_into_left_body())
        })
    }
}
