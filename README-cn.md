# actix-web-ratelimit

一个简单且高度可定制的 actix-web 4 限流中间件。

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)
[![Security](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/security.yml?branch=main&label=security)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[English Documentation](README.md)

## 特性

- **兼容 actix-web 4**: 专为 actix-web 4 构建
- **简单易用**: 最少配置即可使用
- **可扩展存储**: 易于创建自定义存储，已提供内存存储和 Redis 存储
- **高性能**: 高效的滑动窗口算法
- **可定制**: 支持自定义客户端识别和限流处理
- **线程安全**: 使用 DashMap 实现并发请求处理

需要 Rust 1.89 或更新版本。Redis 集成测试覆盖 Redis 7 和 8。

## 快速开始

在你的 `Cargo.toml` 中添加：

```toml
[dependencies]
actix-web-ratelimit = "0.2"
```

Redis 支持默认启用。如果只使用内存存储，请改用以下配置：

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false }
```

## 使用方法

### 基础用法（内存存储）

```rust,no_run
use actix_web::{web, App, HttpServer};
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
use std::sync::Arc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 配置限流：10 秒窗口内允许 3 个请求
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);

    // 创建内存存储，在 HTTP worker 之间共享
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            // 创建并注册限流中间件
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(|| async { "Hello world!" }))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
```

在应用上注册限流中间件，Store 应在 HTTP worker 之间共享。完整代码见 [examples/simple.rs](examples/simple.rs)。

### 高级配置

此示例通过 `.id(...)` 读取 `X-Client-Id` 请求头作为客户端标识；请求头缺失或不是有效文本时，使用 `anonymous`。基础示例仍按连接对端 IP 限流，不使用该请求头。生产环境应先验证请求头，避免客户端伪造标识绕过限流。

```rust
use actix_web::HttpResponse;
use actix_web_ratelimit::config::RateLimitConfig;

let config = RateLimitConfig::default()
    .namespace("api")
    .max_requests(3)
    .window_secs(10)
    // 从请求头提取客户端标识；默认使用不含源端口的连接对端 IP
    .id(|req| {
        req.headers().get("X-Client-Id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("anonymous")
            .to_owned()
    })
    // 自定义超出限额时的响应；默认返回 HTTP 429
    .exceeded(|_id, _config, _req| {
        HttpResponse::TooManyRequests().body("Rate limit exceeded")
    });
```

完整的请求头识别和自定义限流响应代码见 [examples/advance.rs](examples/advance.rs)。

### Redis 存储

`redis` 特性默认启用，快速开始中的依赖配置已包含 Redis 支持。如果需要关闭默认特性并仅显式启用 Redis 支持，请使用：

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false, features = ["redis"] }
```

在启动 HTTP worker 前创建一次 Store：

```rust,ignore
use actix_web_ratelimit::store::RedisStore;
use std::sync::Arc;

// 像基础示例一样，将 store 传入 RateLimit::new(config.clone(), store.clone())。
let store = Arc::new(
    RedisStore::new("redis://127.0.0.1/0").await?
        // Redis 键的自定义前缀
        .with_prefix("myapp:ratelimit:"),
);
```

异步初始化、键前缀及中间件注册的完整代码见 [examples/redis.rs](examples/redis.rs)。

`RedisStore::new(url).await` 默认使用 1 秒超时和 `FailurePolicy::FailOpen`：Redis 出错时放行。可以使用 `with_options` 配置超时，并在后端失败时返回 HTTP 503：

通过 `RedisStoreOptions` 的 `timeout`（例如 `Duration::from_millis(250)`）和 `failure_policy: FailurePolicy::FailClosed` 设置超时及故障策略，并传入 `RedisStore::with_options(url, options).await?`。

初始化错误始终返回。克隆共享异步连接并自动重连；每次检查的总超时包含重连等待。超时时，额度可能已经被记录。Redis Key 包含客户端标识，应使用非敏感 ID，避免直接使用原始 API Key。

## 配置选项

### RateLimitConfig

| 方法 | 描述 | 默认值 |
| ------ | ------ | -------- |
| `namespace(name)` | 逻辑规则名称 | `"default"` |
| `max_requests(usize)` | 时间窗口内最大请求数；零表示全部拒绝 | 10 |
| `window_secs(u64)` | 大于零的时间窗口（秒） | 100 |
| `id(fn)` | 客户端识别函数 | 连接对端 IP，不含源端口 |
| `exceeded(fn)` | 限流超出处理函数 | 429 响应 |

默认标识忽略 `Forwarded` 和 `X-Forwarded-For`。代理场景应通过 `id(...)` 提供校验后的客户端标识，否则会按代理 IP 合并客户端。基于请求头的示例假定应用已验证这些请求头。

同一 Store 或 Redis 前缀内，命名空间、窗口、限额及客户端标识相同的规则共享计数。相同配置的独立路由应使用不同命名空间；不同窗口或限额自动隔离。

通过 `config.validate()` 或 `RateLimit::try_new(config, store)` 处理无效配置；`RateLimit::new` 在配置无效时会 panic。零窗口无效，限额和向上取整后的微秒窗口不得超过 `2^53 - 1`。可通过公开的 `window_secs: Duration` 字段设置亚秒窗口。

### 存储后端

#### MemoryStore

- **优点**: 快速，无外部依赖
- **缺点**: 无法分布式，重启后数据丢失
- **适用场景**: 单实例应用

收到请求时最多每分钟扫描一次，回收不活跃的桶。需要空闲时清理，可在应用定时器中调用 `store.prune_expired()`；扫描成本与桶数量相关，Store 不自动启动后台任务。

#### RedisStore (需要 `redis` 特性)

- **优点**: 分布式，异步访问，原子判定
- **缺点**: 需要 Redis 服务器
- **适用场景**: 多实例应用

Redis 使用服务端时间和 Lua 原子脚本，保证成员唯一。窗口精度为微秒，更小的持续时间向上取整；TTL 精度为毫秒。持久化与故障切换保证取决于 Redis 配置。

#### 自定义 Store

实现 `RateLimitStore::is_limited`，签名为 `fn is_limited<'a>(&'a self, key: &'a str, config: &'a RateLimitConfig) -> BoxFuture<'a, Result<bool, StoreError>>`，其中 `BoxFuture` 来自 `futures-util`。判定和记录必须原子执行，网络 I/O 必须异步。直接检查使用 `store.is_limited(key, &config).await?`：`true` 表示 HTTP 429，存储错误对应 HTTP 503。支持 `Arc`、`Box` 和 trait object。

## 算法

该中间件使用 **滑动窗口** 算法：

1. 从请求中提取客户端标识符
2. 获取客户端存储的请求时间戳
3. 移除时间窗口外的过期时间戳
4. 检查剩余请求数是否已达到限制
5. 如果未超过，记录新时间戳并允许请求
6. 如果超过，调用限流处理函数

窗口范围为 `(now - window, now]`。仅记录已放行的请求；拒绝请求不会延长窗口或续期 Redis TTL。

## 示例

运行示例：

```bash
cargo run --example simple

# 或者
cargo run --example advance
```

然后测试限流：

```bash
# 正常请求
curl http://localhost:8080/

# 通过多次请求触发限流
for i in {1..5}; do echo "$(curl -s http://localhost:8080)\r"; done
# 携带 Header
for i in {1..5}; do echo "$(curl -s -H "X-Client-Id: req_00000001" http://localhost:8080)\r"; done
```

### 测试与基准

```bash
cargo test --locked --all-targets
cargo test --locked --doc
cargo test --locked --all-targets --no-default-features
cargo test --locked --doc --no-default-features
cargo publish --locked --dry-run
REDIS_URL=redis://127.0.0.1:6379/ cargo test --locked --all-targets --all-features -- --include-ignored
cargo bench --locked --bench memory --no-default-features
```

Redis 测试使用独立前缀，不清空数据库。内存微基准覆盖 1、100、10,000 个客户端，其结果不代表 HTTP 吞吐保证。

## [features]

- `redis`: 启用 Redis 存储后端支持（默认启用）

## 许可证

本项目基于 [MIT 许可证](LICENSE) 发布。
