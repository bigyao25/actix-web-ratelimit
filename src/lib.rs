/*!
A simple and highly customizable rate limiting middleware for actix-web 4.

## Features

- **actix-web 4 Compatible**: Built specifically for actix-web 4
- **Simple & Easy to Use**: Minimal configuration required
- **Pluggable Storage**: Support for in-memory and Redis storage backends
- **High Performance**: Efficient sliding window algorithm
- **Customizable**: Custom client identification and rate limit exceeded handlers
- **Thread Safe**: Concurrent request handling with DashMap


## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
actix-web-ratelimit = "0.1"

# Or, for Redis support
actix-web-ratelimit = { version = "0.1", features = ["redis"] }
```

## Usage

### Basic Usage with In-Memory Store

```rust, no_run
# use actix_web::{App, HttpServer, Responder, web};
# use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
# use std::sync::Arc;
#
# async fn index() -> impl Responder {
#     "Hello world!"
# }
#
# #[actix_web::main]
# async fn main() -> std::io::Result<()> {
    // Configure rate limiting: allow 3 requests per 10-second window
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    // Create in-memory store for tracking request timestamps
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            // Create and register the rate limit middleware.
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
# }
```

### Advanced Configuration

```rust, no_run
# use actix_web::HttpResponse;
# use actix_web::{App, HttpServer, Responder, web};
# use actix_web_ratelimit::config::RateLimitConfig;
# use actix_web_ratelimit::{RateLimit, store::MemoryStore};
# use std::sync::Arc;
#
# async fn index() -> impl Responder {
#     "Hello world!"
# }
#
# #[actix_web::main]
# async fn main() -> std::io::Result<()> {
    let store = Arc::new(MemoryStore::new());
    let config = RateLimitConfig::default()
        .max_requests(3)
        .window_secs(10)
        // Extract client identifier from req. It is IP (realip_remote_addr) by default.
        .id(|req| {
            req.headers()
                .get("X-Client-Id")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("anonymous")
                .to_string()
        })
        // Custom handler for rate limit exceeded. It returns a 429 response by default.
        .exceeded(|id, config, _req| {
            HttpResponse::TooManyRequests().body(format!(
                "429 caused: client-id: {}, limit: {}req/{:?}",
                id, config.max_requests, config.window_secs
            ))
        });

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
# }
```

### Redis Store
set feature `redis` enable first:

```toml
actix-web-ratelimit = { version = "0.1", features = [ "redis" ] }
```

then you can use it:

```rust, no_run
# #[cfg(feature = "redis")]
# use actix_web::{App, HttpServer, Responder, web};
# #[cfg(feature = "redis")]
# use actix_web_ratelimit::store::RedisStore;
# #[cfg(feature = "redis")]
# use actix_web_ratelimit::{RateLimit, config::RateLimitConfig};
# #[cfg(feature = "redis")]
# use std::sync::Arc;
#
# #[cfg(feature = "redis")]
# async fn index() -> impl Responder {
#     "Hello world!"
# }
#
# #[cfg(feature = "redis")]
# #[actix_web::main]
# async fn main() -> std::io::Result<()> {
    let store = Arc::new(
        RedisStore::new("redis://127.0.0.1/0")
            .expect("Failed to connect to Redis")
            // Custom prefix for Redis keys
            .with_prefix("myapp:ratelimit:"),
    );
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
# }

# #[cfg(not(feature = "redis"))]
# fn main() {}

```
 */
pub mod config;
pub mod store;

use actix_service::{Service, Transform};
use actix_web::{
    Error,
    body::EitherBody,
    dev::{ServiceRequest, ServiceResponse},
};
use futures_util::future::{LocalBoxFuture, Ready, ok};
use std::{
    sync::Arc,
    task::{Context, Poll},
};

use crate::{config::RateLimitConfig, store::RateLimitStore};

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
    pub fn new(config: RateLimitConfig, store: S) -> Self {
        Self {
            store: Arc::new(store),
            config: Arc::new(config),
        }
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
            service,
            store: self.store.clone(),
            config: self.config.clone(),
        })
    }
}

pub struct RateLimitMiddleware<S> {
    service: S,
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
        let ip = (self.config.get_id)(&req);

        if self.store.is_limited(&ip, &self.config) {
            let res = (self.config.on_exceed)(&ip, &self.config, &req);
            let res = req.into_response(res).map_into_right_body();
            return Box::pin(async { Ok(res) });
        }

        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res.map_into_left_body())
        })
    }
}
