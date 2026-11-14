# actix-web-ratelimit

A simple and highly customizable rate limiting middleware for actix-web 4.

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)
[![Security](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/security.yml?branch=main&label=security)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[中文文档](README-cn.md)

## Features

- **actix-web 4 Compatible**: Built specifically for actix-web 4
- **Simple & Easy to Use**: Minimal configuration required
- **Expandable Store**: easy to create your own store, In-Memory store and Redis store have been provided
- **High Performance**: Efficient sliding window algorithm
- **Customizable**: Custom client identification and rate limit exceeded handlers
- **Thread Safe**: Concurrent request handling with DashMap
- **Observability**: Optional, backend-agnostic event hooks for rate-limit decisions, storage failures, and check latency. See [the example](examples/observability.rs).

Requires Rust 1.89 or newer. Redis integration tests target Redis 7 and 8.

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
actix-web-ratelimit = "0.2"
```

Redis support is enabled by default. For a memory-only build, use this instead:

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false }
```

## Usage

### Basic Usage with In-Memory Store

```rust,no_run
use actix_web::{web, App, HttpServer};
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
use std::sync::Arc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Allow 3 requests per 10-second window
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);

    // Create an in-memory store shared across HTTP workers
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            // Create and register the rate limiting middleware
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(|| async { "Hello world!" }))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
```

Register the rate limiting middleware on your app and share the store across HTTP workers. Full example: [examples/simple.rs](examples/simple.rs).

### Advanced Configuration

This example uses `.id(...)` to read the `X-Client-Id` header as the client identifier, falling back to `anonymous` when the header is missing or is not valid text. The basic example still limits by peer IP and does not use this header. Validate the header in production to prevent clients from bypassing limits with forged identifiers.

```rust
use actix_web::HttpResponse;
use actix_web_ratelimit::config::RateLimitConfig;

let config = RateLimitConfig::default()
    .namespace("api")
    .max_requests(3)
    .window_secs(10)
    // Extract a client identifier; the default is the peer IP without its source port
    .id(|req| {
        req.headers().get("X-Client-Id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("anonymous")
            .to_owned()
    })
    // Customize the limit-exceeded response; the default is HTTP 429
    .exceeded(|_id, _config, _req| {
        HttpResponse::TooManyRequests().body("Rate limit exceeded")
    });
```

For header-based identification and a custom rate limit response, see [examples/advance.rs](examples/advance.rs).

### Optional Quota Response Headers

Response headers are disabled by default. Enable `RateLimit-Policy`, `RateLimit` and, on HTTP 429 when available, `Retry-After`:

```rust
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};

let config = RateLimitConfig::default().namespace("api").max_requests(100).window_secs(60);
let middleware = RateLimit::new(config, MemoryStore::new()).response_headers(true);
```

Example response headers:

```http
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=42
```

When the quota is exhausted, a response can also include:

```http
Retry-After: 12
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=0
```

### Redis Store

The `redis` feature is enabled by default, so the Quick Start dependency already includes Redis support. To disable default features and enable only Redis support explicitly, use:

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false, features = ["redis"] }
```

Create the store once, before starting HTTP workers:

```rust,ignore
use actix_web_ratelimit::store::RedisStore;
use std::sync::Arc;

// Register with RateLimit::new(config.clone(), store.clone()), as above.
let store = Arc::new(
    RedisStore::new("redis://127.0.0.1/0").await?
        // Custom prefix for Redis keys
        .with_prefix("myapp:ratelimit:"),
);
```

For asynchronous initialization, key prefixes and middleware registration, see [examples/redis.rs](examples/redis.rs).

`RedisStore::new(url).await` uses a one-second deadline and `FailurePolicy::FailOpen`: Redis errors allow requests. Use `with_options` to configure the deadline and return HTTP 503 on backend failures:

Pass `RedisStoreOptions` with `timeout` (for example, `Duration::from_millis(250)`) and `failure_policy: FailurePolicy::FailClosed` to `RedisStore::with_options(url, options).await?`.

Initialization errors are always returned. Clones reuse an asynchronous connection and reconnect automatically. Each check has a total deadline including reconnect waits; a timeout may occur after quota has already been recorded. Redis keys contain the client identifier, so use non-secret IDs rather than raw API keys.

## Configuration Options

### RateLimitConfig

| Method | Description | Default |
| ------ | ------ | -------- |
| `namespace(name)` | Logical rule name | `"default"` |
| `max_requests(usize)` | Maximum requests per window; zero denies all | 10 |
| `window_secs(u64)` | Positive time window in seconds | 100 |
| `id(fn)` | Client identification function | Peer IP without source port |
| `exceeded(fn)` | Rate limit exceeded handler | 429 response |

The default identifier ignores `Forwarded` and `X-Forwarded-For`. Behind a proxy, supply a validated identity extractor through `id(...)`; otherwise clients share the proxy's IP bucket. Header-based examples assume the application validates those headers.

Equal namespace, window, quota and client ID share a bucket in the same store or Redis prefix. Use different namespaces for independent routes with equal quotas; different windows or quotas are isolated automatically.

Use `config.validate()` or `RateLimit::try_new(config, store)` to handle invalid settings; `RateLimit::new` panics on invalid configuration. Zero windows are invalid. The request limit and rounded microsecond window must not exceed `2^53 - 1`. Sub-second windows can be set through the public `window_secs: Duration` field.

### Storage Backends

#### MemoryStore

- **Pros**: Fast, no external dependencies
- **Cons**: Not distributed, data lost on restart
- **Use case**: Single instance applications

Inactive buckets are swept at most once per minute when requests arrive. Call `store.prune_expired()` from an application timer for cleanup while idle. Sweeps scan all buckets; no background task is started automatically.

#### RedisStore (requires `redis` feature)

- **Pros**: Distributed, asynchronous, atomic admission
- **Cons**: Requires Redis server
- **Use case**: Multi-instance applications

Redis uses server time and an atomic Lua script with unique members. Window precision is one microsecond, rounding up smaller durations; TTL precision is one millisecond. Persistence and failover guarantees depend on Redis configuration.

#### Custom Store

Implement `RateLimitStore::is_limited` with `fn is_limited<'a>(&'a self, key: &'a str, config: &'a RateLimitConfig) -> BoxFuture<'a, Result<bool, StoreError>>` (`BoxFuture` is from `futures-util`). Admission must be atomic and network I/O asynchronous. Direct checks use `store.is_limited(key, &config).await?`: `true` means HTTP 429; storage errors produce HTTP 503. `Arc` and `Box` trait objects are supported.

## Algorithm

This middleware uses a **sliding window** algorithm:

1. Extract client identifier from request
2. Retrieve stored request timestamps for the client
3. Remove expired timestamps outside the time window
4. Check if remaining request count has reached the limit
5. If not exceeded, record new timestamp and allow request
6. If exceeded, call the rate limit handler

The window is `(now - window, now]`. Only admitted requests are recorded; rejected requests do not extend the window or renew the Redis TTL.

## Examples

Run the example:

```bash
cargo run --example simple

# Or
cargo run --example advance
```

Then test the rate limiting:

```bash
# This should work
curl http://localhost:8080/

# Exceed rate limit by making many requests
for i in {1..5}; do echo "$(curl -s http://localhost:8080)\r"; done
# With Header
for i in {1..5}; do echo "$(curl -s -H "X-Client-Id: req_00000001" http://localhost:8080)\r"; done
```

### Tests and Benchmarks

```bash
cargo test --locked --all-targets
cargo test --locked --doc
cargo test --locked --all-targets --no-default-features
cargo test --locked --doc --no-default-features
cargo publish --locked --dry-run
REDIS_URL=redis://127.0.0.1:6379/ cargo test --locked --all-targets --all-features -- --include-ignored
cargo bench --locked --bench memory --no-default-features
cargo bench --locked --bench middleware --no-default-features
```

Redis tests use isolated prefixes and never flush the database. Microbenchmarks cover memory checks and optional response headers; their results are not HTTP throughput guarantees.

## [features]

- `redis`: Enables Redis storage backend support (enabled by default)

## License

This project is licensed under the [MIT License](LICENSE).
