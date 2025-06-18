# actix-web-ratelimit

一个简单且高度可定制的 actix-web 4 限流中间件。

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[English Documentation](README.md)

## 特性

- **兼容 actix-web 4**: 专为 actix-web 4 构建
- **简单易用**: 最少配置即可使用
- **可插拔存储**: 支持内存和 Redis 存储后端
- **高性能**: 高效的滑动窗口算法
- **可定制**: 支持自定义客户端识别和限流处理
- **线程安全**: 使用 DashMap 实现并发请求处理

## 快速开始

在你的 `Cargo.toml` 中添加：

```toml
[dependencies]
actix-web-ratelimit = "0.1"

# 启用 Redis 支持
actix-web-ratelimit = { version = "0.1", features = ["redis"] }
```

## 使用方法

### 基础用法（内存存储）

```rust
use actix_web::{web, App, HttpServer, Responder};
use actix_web_ratelimit::{RateLimit, RateLimitConfig, store::MemoryStore};

async fn index() -> impl Responder {
    "Hello world!"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let store = MemoryStore::new();
    let config = RateLimitConfig::default()
        .max_requests(10)
        .window_secs(60);

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
```

### 高级配置

```rust
use actix_web::{HttpResponse, web};
use actix_web_ratelimit::{RateLimit, RateLimitConfig, store::MemoryStore};

let store = MemoryStore::new();
let config = RateLimitConfig::default()
    .max_requests(100)
    .window_secs(3600)
    .id(|req| {
        // 自定义客户端识别
        req.headers()
            .get("X-API-Key")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("anonymous")
            .to_string()
    })
    .exceeded(|_id, _config, _req| {
        // 自定义限流超出响应
        HttpResponse::TooManyRequests()
            .json(serde_json::json!({
                "error": "请求过于频繁",
                "retry_after": 60
            }))
    });

App::new().wrap(RateLimit::new(config, store))
```

### Redis 存储

```rust
use actix_web_ratelimit::{RateLimit, RateLimitConfig, store::RedisStore};

#[cfg(feature = "redis")]
{
    let store = RedisStore::new("redis://127.0.0.1/")
        .expect("连接 Redis 失败")
        .with_prefix("myapp:");
    
    let config = RateLimitConfig::default()
        .max_requests(1000)
        .window_secs(3600);

    App::new().wrap(RateLimit::new(config, store))
}
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
for i in {1..15}; do curl http://localhost:8080/; done
```

## 特性标志

- `redis`: 启用 Redis 存储后端支持

## 许可证

本项目基于 [MIT 许可证](LICENSE) 发布。