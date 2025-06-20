# actix-web-ratelimit

一个简单且高度可定制的 actix-web 4 限流中间件。

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)

[English Documentation](README.md)

## 特性

- **兼容 actix-web 4**: 专为 actix-web 4 构建
- **简单易用**: 最少配置即可使用
- **可扩展存储**: 易于创建自定义存储，已提供内存存储和 Redis 存储
- **高性能**: 高效的滑动窗口算法
- **可定制**: 支持自定义客户端识别和限流处理
- **线程安全**: 使用 DashMap 实现并发请求处理

## 快速开始

在你的 `Cargo.toml` 中添加：

```toml
[dependencies]
actix-web-ratelimit = "0.1"

# 或者，启用 Redis 支持
actix-web-ratelimit = { version = "0.1", features = ["redis"] }
```

## 使用方法

### 基础用法（内存存储）

```rust
    // 配置限流：10 秒窗口内允许 3 个请求
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    // 创建内存存储用于跟踪请求时间戳
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            // 创建并注册限流中间件
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
```

### 高级配置

```rust
    let store = Arc::new(MemoryStore::new());
    let config = RateLimitConfig::default()
        .max_requests(3)
        .window_secs(10)
        // 从请求中提取客户端标识符。默认为 IP (realip_remote_addr)。
        .id(|req| {
            req.headers()
                .get("X-Client-Id")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("anonymous")
                .to_string()
        })
        // 限流超出时的自定义处理器。默认返回 429 响应。
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

### Redis 存储

首先启用 `redis` 特性：
```toml
actix-web-ratelimit = { version = "0.1", features = [ "redis" ] }
```
然后你可以使用它：
```rust
    let store = Arc::new(
        RedisStore::new("redis://127.0.0.1/0")
            .expect("连接 Redis 失败")
            // Redis 键的自定义前缀
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

## 配置选项

### RateLimitConfig

| 方法 | 描述 | 默认值 |
|------|------|--------|
| `max_requests(usize)` | 时间窗口内最大请求数 | 10 |
| `window_secs(u64)` | 时间窗口（秒） | 100 |
| `id(fn)` | 客户端识别函数 | IP 地址 |
| `exceeded(fn)` | 限流超出处理函数 | 429 响应 |

### 存储后端

#### MemoryStore
- **优点**: 快速，无外部依赖
- **缺点**: 无法分布式，重启后数据丢失
- **适用场景**: 单实例应用

#### RedisStore (需要 `redis` 特性)
- **优点**: 分布式，持久化，可扩展
- **缺点**: 需要 Redis 服务器
- **适用场景**: 多实例应用

## 算法

该中间件使用 **滑动窗口** 算法：

1. 从请求中提取客户端标识符
2. 获取客户端存储的请求时间戳
3. 移除时间窗口外的过期时间戳
4. 检查剩余请求数是否超过限制
5. 如果未超过，记录新时间戳并允许请求
6. 如果超过，调用限流处理函数

## 示例

运行示例：

```bash
cargo run --example simple
```

然后测试限流：

```bash
# 正常请求
curl http://localhost:8080/

# 通过多次请求触发限流
for i in {1..5}; do echo "$(curl -s http://localhost:8080)\r"; done
```

## 特性标志

- `redis`: 启用 Redis 存储后端支持

## 许可证

本项目基于 [MIT 许可证](LICENSE) 发布。