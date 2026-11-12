# actix-web-ratelimit

An accurate, high-performance and easy-to-customize rate limiting middleware for actix-web 4, with in-memory and Redis stores.

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)
[![Security](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/security.yml?branch=main&label=security)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[中文文档](README-cn.md)

## Features

- **Accurate sliding window**: A more precise sliding window algorithm that strictly keeps every time window within the quota. See [How It Works](#how-it-works).
- **High performance**: Fine-grained per-client locking with no global lock; Redis decides in a single round trip. End-to-end load tests show throughput on par with, or better than, leading alternatives.
- **Atomic decisions**: Both memory and Redis stores check and record atomically, staying exact across multiple Redis-backed instances.
- **Standard response headers**: Optional IETF draft `RateLimit-Policy` / `RateLimit` plus `Retry-After`, so clients can back off adaptively.
- **Easy to start, easy to customize**: A few lines to integrate; custom client identity, exceeded responses, namespaces and Redis failure policy.
- **Extensible storage**: Built-in memory and Redis stores; implement `RateLimitStore` for your own backend.
- **Observability**: Optional, backend-agnostic event hooks for rate-limit decisions, storage failures, and check latency. See [the example](examples/observability.rs).
- **Well tested and benchmarked**: Boundary, concurrency, multi-worker and Redis 7/8 integration tests; micro-benchmarks plus Docker-based end-to-end load tests. See [benchmarks](benchmarks/README.md).

Requires Rust 1.89 or newer.

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
actix-web-ratelimit = "0.2"
```

Redis support is enabled by default. For a memory-only build, disable default features:

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

Create the store once, outside `HttpServer::new`, and share it across all HTTP workers; otherwise each worker counts separately. Full example: [examples/simple.rs](examples/simple.rs).

### Advanced Configuration

By default, clients are identified by peer IP. This example uses `.id(...)` to read the `X-Client-Id` header instead (falling back to `anonymous` when missing or not valid text) and customizes the exceeded response:

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

> Validate such headers in production so clients cannot bypass limits with forged identifiers.

Full example: [examples/advance.rs](examples/advance.rs).

### Quota Response Headers

Response headers are disabled by default. When enabled, responses carry `RateLimit-Policy` and `RateLimit`, and HTTP 429 responses include `Retry-After` when it can be determined:

```rust
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};

let config = RateLimitConfig::default().namespace("api").max_requests(100).window_secs(60);
let middleware = RateLimit::new(config, MemoryStore::new()).response_headers(true);
```

Normal response:

```http
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=42
```

Quota exhausted:

```http
Retry-After: 12
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=0
```

### Redis Store

The `redis` feature is enabled by default. With default features disabled, enable it explicitly:

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false, features = ["redis"] }
```

Create the store once before starting HTTP workers, and pass it to `RateLimit::new` as in the basic example:

```rust,ignore
use actix_web_ratelimit::store::RedisStore;
use std::sync::Arc;

let store = Arc::new(
    RedisStore::new("redis://127.0.0.1/0").await?
        // Custom prefix for Redis keys
        .with_prefix("myapp:ratelimit:"),
);
```

Full example: [examples/redis.rs](examples/redis.rs).

`RedisStore::new` uses a one-second timeout and `FailurePolicy::FailOpen` (requests are admitted when Redis fails). To change the timeout or return HTTP 503 on Redis failures, use `with_options`:

```rust,ignore
use actix_web_ratelimit::store::{FailurePolicy, RedisStore, RedisStoreOptions};
use std::time::Duration;

let options = RedisStoreOptions {
    timeout: Duration::from_millis(250),
    failure_policy: FailurePolicy::FailClosed,
};
let store = RedisStore::with_options("redis://127.0.0.1/0", options).await?;
```

Notes:

- Initialization errors are always returned, regardless of failure policy.
- Clones share one asynchronous connection that reconnects automatically; the timeout includes reconnect waits.
- When a check times out, the request may already have been counted.
- Redis keys contain the client identifier, so use non-secret IDs rather than raw API keys.

## Configuration

### RateLimitConfig

| Method | Description | Default |
| ------ | ------ | -------- |
| `namespace(name)` | Rule name used to isolate counters | `"default"` |
| `max_requests(usize)` | Maximum requests per window; `0` denies all | 10 |
| `window_secs(u64)` | Window length in seconds; must be positive | 100 |
| `id(fn)` | Client identification function | Peer IP (without port) |
| `exceeded(fn)` | Limit-exceeded response function | HTTP 429 |

- **Behind a proxy**: The default identifier ignores `Forwarded` and `X-Forwarded-For`. Behind a reverse proxy, supply a validated identity through `id(...)`; otherwise all clients share the proxy IP's quota.
- **Counter isolation**: Within one store (or Redis prefix), rules with equal namespace, window, quota and client ID share a counter. Different windows or quotas are isolated automatically; use different namespaces for routes with equal settings that need independent counters.
- **Validation**: `RateLimit::new` panics on invalid configuration; use `config.validate()` or `RateLimit::try_new(config, store)` to handle errors yourself. The quota and the window rounded up to microseconds must not exceed `2^53 - 1`.
- **Sub-second windows**: Set the public `window_secs: Duration` field directly.

### Storage Backends

| | MemoryStore | RedisStore (requires `redis` feature) |
| --- | --- | --- |
| Use case | Single-instance applications | Multi-instance / distributed applications |
| Pros | Very fast, no external dependencies | Counters shared across instances, async I/O |
| Limits | Not shared across processes, cleared on restart | Requires a Redis server |

**MemoryStore cleanup**: While requests arrive, inactive counters are swept at most once per minute. No background task is started; to reclaim memory while idle, call `store.prune_expired()` from an application timer. Its cost scales with the number of clients.

**RedisStore precision**: Window precision is one microsecond (smaller values round up); TTL precision is one millisecond. Persistence and failover behavior depend on your Redis configuration.

### Custom Store

Implement `RateLimitStore::is_limited`:

```rust,ignore
fn is_limited<'a>(
    &'a self,
    key: &'a str,
    config: &'a RateLimitConfig,
) -> BoxFuture<'a, Result<bool, StoreError>>; // BoxFuture is from futures-util
```

- `Ok(true)` produces HTTP 429; an error produces HTTP 503.
- Checking and recording must be atomic, and network I/O must be asynchronous.
- `Arc`, `Box` and trait objects are supported.

## How It Works

### Sliding Window Log

Each client keeps a list of admitted request timestamps. When a request arrives:

1. Extract the client identifier via `id(...)` and combine it with namespace, window and quota into a counter key;
2. Drop timestamps outside the window `(now - window, now]`;
3. If the remaining count is below the quota, record the current timestamp and admit the request;
4. Otherwise, call the exceeded handler (HTTP 429 by default).

Rejected requests are never recorded, so they do not extend the window or renew the Redis TTL.

### Why It Is More Accurate

| Algorithm | Typical issue | This crate |
| --- | --- | --- |
| Fixed window | Can admit nearly 2× the quota around window boundaries | The window slides with every request; no boundary bursts |
| Bucketed / approximate sliding window | Aggregates or weights time buckets, introducing error | Records every timestamp; counts are exact |
| Token bucket / GCRA | Controls average rate and burst, not "at most N in any window" | Directly guarantees at most N requests in any window |

The trade-off is storing up to `max_requests` timestamps per active client, which is small for typical API quotas.

### Concurrency and Consistency

- **MemoryStore**: Built on DashMap with per-key locking; the clock is read and the decision made under the lock, so different clients never block each other.
- **RedisStore**: A Lua script atomically prunes, counts and records in one round trip. Time comes from the Redis server and is kept monotonic, so instances are unaffected by local clock skew.

## Examples

```bash
cargo run --example simple
cargo run --example advance
cargo run --example redis          # requires a local Redis
cargo run --example observability
```

Verify the limiter:

```bash
# Repeated requests return 429 once the quota is exhausted
for i in {1..5}; do curl -s -w " %{http_code}\n" http://localhost:8080/; done

# With a client identifier (for the advance example)
for i in {1..5}; do curl -s -w " %{http_code}\n" -H "X-Client-Id: req_00000001" http://localhost:8080/; done
```

## Tests and Benchmarks

```bash
# Unit and integration tests (with / without default features)
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features

# Doc tests
cargo test --locked --doc

# Redis integration tests
REDIS_URL=redis://127.0.0.1:6379/ cargo test --locked --all-targets --all-features -- --include-ignored

# Micro-benchmarks
cargo bench --locked --bench memory --no-default-features
cargo bench --locked --bench middleware --no-default-features
```

Redis tests use isolated prefixes and never flush the database. Micro-benchmarks measure per-check cost, not HTTP throughput; see [benchmarks/README.md](benchmarks/README.md) for end-to-end load tests against similar crates.

## Cargo Features

- `redis`: Enables the Redis storage backend (enabled by default)

## License

This project is licensed under the [MIT License](LICENSE).
