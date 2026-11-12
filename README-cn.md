# actix-web-ratelimit

一个精确、高性能且易于定制的 actix-web 4 限流中间件，支持内存和 Redis 存储。

[![Crates.io](https://img.shields.io/crates/v/actix-web-ratelimit.svg)](https://crates.io/crates/actix-web-ratelimit)
[![Documentation](https://docs.rs/actix-web-ratelimit/badge.svg)](https://docs.rs/actix-web-ratelimit)
[![CI](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/CI.yml?branch=main)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/CI.yml)
[![Security](https://img.shields.io/github/actions/workflow/status/bigyao25/actix-web-ratelimit/security.yml?branch=main&label=security)](https://github.com/bigyao25/actix-web-ratelimit/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[English Documentation](README.md)

## 特性

- **精确的滑动窗口**：采用更精准的滑动窗口算法，严格保证任意时间窗口内不超过限额。参见[运行机制](#运行机制)。
- **高性能**：按客户端细粒度加锁，无全局锁；Redis 单次往返即完成判定。端到端压测中吞吐与同类主流库持平甚至更优。
- **原子判定**：内存与 Redis 存储均原子执行判定和记录，Redis 多实例部署同样精确。
- **标准响应头**：可选输出 IETF 草案 `RateLimit-Policy` / `RateLimit` 及 `Retry-After`，便于客户端自适应退避。
- **开箱即用，按需定制**：几行代码即可接入；支持自定义客户端标识、超限响应、命名空间和 Redis 故障策略。
- **可扩展存储**：内置内存和 Redis 存储，实现 `RateLimitStore` 即可接入自定义后端。
- **可观测性**：可选、平台无关的事件接口，覆盖限流判定、存储故障和检查耗时。参见[示例](examples/observability.rs)。
- **测试与基准完备**：覆盖边界、并发、多 worker 及 Redis 7/8 集成测试；提供微基准和基于 Docker 的端到端压测。参见 [benchmarks](benchmarks/README.md)。

需要 Rust 1.89 或更新版本。

## 快速开始

在 `Cargo.toml` 中添加：

```toml
[dependencies]
actix-web-ratelimit = "0.2"
```

Redis 支持默认启用。仅使用内存存储时，可关闭默认特性：

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

Store 应在 `HttpServer::new` 之外创建一次，并在所有 HTTP worker 之间共享；否则每个 worker 会各自计数。完整代码见 [examples/simple.rs](examples/simple.rs)。

### 高级配置

默认按连接对端 IP 限流。下例通过 `.id(...)` 改用 `X-Client-Id` 请求头作为客户端标识（缺失或非有效文本时使用 `anonymous`），并自定义超限响应：

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

> 生产环境应先校验请求头，避免客户端伪造标识绕过限流。

完整代码见 [examples/advance.rs](examples/advance.rs)。

### 额度响应头

响应头默认关闭。开启后输出 `RateLimit-Policy`、`RateLimit`，并在 HTTP 429 时尽可能附带 `Retry-After`：

```rust
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};

let config = RateLimitConfig::default().namespace("api").max_requests(100).window_secs(60);
let middleware = RateLimit::new(config, MemoryStore::new()).response_headers(true);
```

正常响应：

```http
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=42
```

额度耗尽时：

```http
Retry-After: 12
RateLimit-Policy: "api";q=100;w=60
RateLimit: "api";r=0
```

### Redis 存储

`redis` 特性默认启用。关闭默认特性时，可显式启用：

```toml
[dependencies]
actix-web-ratelimit = { version = "0.2", default-features = false, features = ["redis"] }
```

在启动 HTTP worker 前创建一次 Store，并像基础示例一样传入 `RateLimit::new`：

```rust,ignore
use actix_web_ratelimit::store::RedisStore;
use std::sync::Arc;

let store = Arc::new(
    RedisStore::new("redis://127.0.0.1/0").await?
        // Redis 键的自定义前缀
        .with_prefix("myapp:ratelimit:"),
);
```

完整代码见 [examples/redis.rs](examples/redis.rs)。

`RedisStore::new` 默认使用 1 秒超时和 `FailurePolicy::FailOpen`（Redis 出错时放行）。如需调整超时，或在 Redis 故障时返回 HTTP 503，使用 `with_options`：

```rust,ignore
use actix_web_ratelimit::store::{FailurePolicy, RedisStore, RedisStoreOptions};
use std::time::Duration;

let options = RedisStoreOptions {
    timeout: Duration::from_millis(250),
    failure_policy: FailurePolicy::FailClosed,
};
let store = RedisStore::with_options("redis://127.0.0.1/0", options).await?;
```

注意事项：

- 初始化失败始终返回错误，不受故障策略影响。
- 克隆的 Store 共享同一异步连接，断线后自动重连；超时时间包含重连等待。
- 检查超时时，本次请求可能已被计入额度。
- Redis Key 包含客户端标识，应使用非敏感 ID，不要直接使用原始 API Key。

## 配置选项

### RateLimitConfig

| 方法 | 描述 | 默认值 |
| ------ | ------ | -------- |
| `namespace(name)` | 规则名称，用于隔离计数 | `"default"` |
| `max_requests(usize)` | 窗口内最大请求数；`0` 表示全部拒绝 | 10 |
| `window_secs(u64)` | 窗口长度（秒），必须大于零 | 100 |
| `id(fn)` | 客户端标识函数 | 连接对端 IP（不含端口） |
| `exceeded(fn)` | 超限响应函数 | HTTP 429 |

- **代理场景**：默认标识忽略 `Forwarded` 和 `X-Forwarded-For`。位于反向代理之后时，应通过 `id(...)` 提供校验后的客户端标识，否则所有客户端会共享代理 IP 的额度。
- **计数隔离**：同一 Store（或同一 Redis 前缀）内，命名空间、窗口、限额和客户端标识均相同的规则共享计数。窗口或限额不同的规则自动隔离；配置相同但需独立计数的路由，应使用不同命名空间。
- **配置校验**：`RateLimit::new` 在配置无效时 panic；如需自行处理，使用 `config.validate()` 或 `RateLimit::try_new(config, store)`。限额和向上取整到微秒的窗口均不得超过 `2^53 - 1`。
- **亚秒窗口**：可直接设置公开字段 `window_secs: Duration`。

### 存储后端

| | MemoryStore | RedisStore（需 `redis` 特性） |
| --- | --- | --- |
| 适用场景 | 单实例应用 | 多实例 / 分布式应用 |
| 优点 | 极快，无外部依赖 | 多实例共享计数，异步访问 |
| 限制 | 不跨进程共享，重启后清空 | 需要 Redis 服务 |

**MemoryStore 清理**：有请求到达时，最多每分钟扫描一次并回收不活跃的计数。Store 不会自动启动后台任务；如需在空闲时回收，可在应用定时器中调用 `store.prune_expired()`，其开销与客户端数量成正比。

**RedisStore 精度**：窗口精度为微秒（更小的值向上取整），TTL 精度为毫秒。持久化与故障切换行为取决于 Redis 自身配置。

### 自定义 Store

实现 `RateLimitStore::is_limited` 即可：

```rust,ignore
fn is_limited<'a>(
    &'a self,
    key: &'a str,
    config: &'a RateLimitConfig,
) -> BoxFuture<'a, Result<bool, StoreError>>; // BoxFuture 来自 futures-util
```

- 返回 `Ok(true)` 时响应 HTTP 429，返回错误时响应 HTTP 503。
- 判定与记录必须原子执行，网络 I/O 必须异步。
- 支持通过 `Arc`、`Box` 及 trait object 使用。

## 运行机制

### 滑动窗口日志

每个客户端维护一份已放行请求的时间戳列表。请求到达时：

1. 通过 `id(...)` 提取客户端标识，与命名空间、窗口、限额组合成计数键；
2. 移除窗口 `(now - window, now]` 之外的时间戳；
3. 剩余数量未达限额时，记录当前时间戳并放行；
4. 否则调用超限响应函数（默认 HTTP 429）。

被拒绝的请求不会被记录，因此不会延长窗口，也不会续期 Redis TTL。

### 为什么更精确

| 算法 | 典型问题 | 本库 |
| --- | --- | --- |
| 固定窗口 | 两个窗口交界处可在短时间内放行近 2 倍限额 | 窗口随每个请求连续滑动，无边界突刺 |
| 分桶 / 近似滑动窗口 | 按时间桶聚合或加权估算，存在误差 | 逐条记录时间戳，计数精确 |
| 令牌桶 / GCRA | 控制的是平均速率与突发量，而非“任意窗口内最多 N 次” | 直接保证任意窗口内不超过 N 次 |

代价是每个活跃客户端最多保存 `max_requests` 个时间戳，对常见的 API 限额而言开销很小。

### 并发与一致性

- **MemoryStore**：基于 DashMap，按计数键加锁，在锁内读取时钟并完成判定，不同客户端之间互不阻塞。
- **RedisStore**：使用 Lua 脚本在一次往返内原子完成清理、计数和记录；时间取自 Redis 服务端并保持单调，多实例之间不受本地时钟偏差影响。

## 示例

```bash
cargo run --example simple
cargo run --example advance
cargo run --example redis          # 需要本地 Redis
cargo run --example observability
```

验证限流效果：

```bash
# 连续请求，超过限额后返回 429
for i in {1..5}; do curl -s -w " %{http_code}\n" http://localhost:8080/; done

# 携带客户端标识（适用于 advance 示例）
for i in {1..5}; do curl -s -w " %{http_code}\n" -H "X-Client-Id: req_00000001" http://localhost:8080/; done
```

## 测试与基准

```bash
# 单元与集成测试（含 / 不含默认特性）
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features

# 文档测试
cargo test --locked --doc

# Redis 集成测试
REDIS_URL=redis://127.0.0.1:6379/ cargo test --locked --all-targets --all-features -- --include-ignored

# 微基准
cargo bench --locked --bench memory --no-default-features
cargo bench --locked --bench middleware --no-default-features
```

Redis 测试使用独立前缀，不会清空数据库。微基准衡量单次检查开销，不代表 HTTP 吞吐；端到端压测与同类库对比见 [benchmarks/README.md](benchmarks/README.md)。

## Cargo 特性

- `redis`：启用 Redis 存储后端（默认启用）

## 许可证

本项目基于 [MIT 许可证](LICENSE) 发布。
