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
    store::RateLimitStore,
};

pub struct RateLimit<S>
where
    S: RateLimitStore,
{
    store: Arc<S>,
    config: Arc<RateLimitConfig>,
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
        })
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
            store: self.store.clone(),
            config: self.config.clone(),
        })
    }
}

pub struct RateLimitMiddleware<S> {
    service: Rc<S>,
    store: Arc<dyn RateLimitStore>,
    config: Arc<RateLimitConfig>,
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
        Box::pin(async move {
            let id = (config.get_id)(&req);
            match store.is_limited(&id, &config).await {
                Ok(true) => {
                    let response = (config.on_exceed)(&id, &config, &req);
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
                Ok(false) => {}
            }
            let res = service.call(req).await?;
            Ok(res.map_into_left_body())
        })
    }
}
