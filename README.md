# actix-web-ratelimit

A simple and highly customizable rate limiting middleware for actix-web 4.

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)

[中文文档](README-cn.md)

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

```rust
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
```

### Advanced Configuration

```rust
    let store = Arc::new(MemoryStore::new());
    let config = RateLimitConfig::default()
        .max_requests(3)
        .window_secs(10)
        .id(|req| {
            // custom client identification
            req.headers()
                .get("X-Client-Id")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("anonymous")
                .to_string()
        })
            // custom response when rate limit exceeded
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
```

### Redis Store
first set feature `redis` enable:
```toml
actix-web-ratelimit = { version = "0.1", features = [ "redis" ] }
```
then you can use it:
```rust
    let store = Arc::new(
        RedisStore::new("redis://127.0.0.1/0")
            .expect("Failed to connect to Redis")
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
```

## Configuration Options

### RateLimitConfig

| Method | Description | Default |
|--------|-------------|---------|
| `max_requests(usize)` | Maximum requests per window | 10 |
| `window_secs(u64)` | Time window in seconds | 100 |
| `id(fn)` | Client identification function | IP address |
| `exceeded(fn)` | Rate limit exceeded handler | 429 response |

### Storage Backends

#### MemoryStore
- **Pros**: Fast, no external dependencies
- **Cons**: Not distributed, data lost on restart
- **Use case**: Single instance applications

#### RedisStore (requires `redis` feature)
- **Pros**: Distributed, persistent, scalable
- **Cons**: Requires Redis server
- **Use case**: Multi-instance applications

## Algorithm

This middleware uses a **sliding window** algorithm:

1. Extract client identifier from request
2. Retrieve stored request timestamps for the client
3. Remove expired timestamps outside the time window
4. Check if remaining request count exceeds the limit
5. If not exceeded, record new timestamp and allow request
6. If exceeded, call the rate limit handler

## Examples

Run the example:

```bash
cargo run --example simple
```

Then test the rate limiting:

```bash
# This should work
curl http://localhost:8080/

# Exceed rate limit by making many requests
for i in {1..5}; do echo "$(curl -s http://localhost:8080)\r"; done
```

## Features

- `redis`: Enables Redis storage backend support

## License

This project is licensed under the [MIT License](LICENSE).
