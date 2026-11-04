#![cfg(feature = "redis")]
use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{FailurePolicy, RateLimitStore, RedisStore, RedisStoreOptions},
};
use futures_util::{
    FutureExt,
    future::{LocalBoxFuture, join_all},
};
use redis::aio::MultiplexedConnection;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    prefix: String,
    connection: MultiplexedConnection,
    store: RedisStore,
}

impl Fixture {
    async fn new(policy: FailurePolicy) -> Self {
        let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
        let prefix = format!(
            "ratelimit:test:{}:{}:{}:",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let connection = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let store = RedisStore::with_options(
            &url,
            RedisStoreOptions {
                failure_policy: policy,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .with_prefix(&prefix);
        Self {
            prefix,
            connection,
            store,
        }
    }

    fn key(&self, client: &str, config: &RateLimitConfig) -> String {
        format!(
            "{}v2:{}:{}:{}:{}:{}:{}",
            self.prefix,
            config.namespace.len(),
            config.namespace,
            config.window_secs.as_nanos(),
            config.max_requests,
            client.len(),
            client
        )
    }

    async fn cleanup(&mut self) {
        // Only delete keys in this test's unique prefix. Never flush the shared database.
        let mut keys = Vec::new();
        let mut cursor = 0_u64;
        loop {
            let (next, page): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(format!("{}*", self.prefix))
                .arg("COUNT")
                .arg(100)
                .query_async(&mut self.connection)
                .await
                .unwrap();
            keys.extend(page);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        if !keys.is_empty() {
            redis::cmd("DEL")
                .arg(keys)
                .query_async::<usize>(&mut self.connection)
                .await
                .unwrap();
        }
    }
}

// Always remove this test's keys, including when a regression assertion panics.
async fn with_fixture(
    test_policy: FailurePolicy,
    test: impl for<'a> FnOnce(&'a mut Fixture) -> LocalBoxFuture<'a, ()>,
) {
    let mut fixture = Fixture::new(test_policy).await;
    let outcome = std::panic::AssertUnwindSafe(test(&mut fixture))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[actix_web::test]
#[ignore = "requires Redis; run cargo test --test redis -- --ignored"]
async fn burst_admits_exactly_the_configured_limit() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let config = RateLimitConfig::default().max_requests(10);
            let mut allowed = 0;
            for _ in 0..80 {
                allowed += usize::from(!f.store.is_limited("client", &config).await.unwrap());
            }
            assert_eq!(allowed, 10);
            assert_eq!(
                redis::cmd("ZCARD")
                    .arg(f.key("client", &config))
                    .query_async::<usize>(&mut f.connection)
                    .await
                    .unwrap(),
                10
            );
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn independent_connections_share_one_atomic_quota() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
            let second = RedisStore::new(&url).await.unwrap().with_prefix(&f.prefix);
            let config = RateLimitConfig::default().max_requests(37);
            let results = join_all((0..256).map(|i| {
                let store = if i % 2 == 0 { &f.store } else { &second };
                let config = &config;
                async move { !store.is_limited("shared", config).await.unwrap() }
            }))
            .await;
            assert_eq!(results.into_iter().filter(|allowed| *allowed).count(), 37);
            assert_eq!(
                redis::cmd("ZCARD")
                    .arg(f.key("shared", &config))
                    .query_async::<usize>(&mut f.connection)
                    .await
                    .unwrap(),
                37
            );
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn same_timestamp_members_are_unique_even_when_server_clock_moves_backwards() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let config = RateLimitConfig::default().max_requests(32);
            let key = f.key("shared", &config);
            let time: (u64, u64) = redis::cmd("TIME")
                .query_async(&mut f.connection)
                .await
                .unwrap();
            let future = time.0 * 1_000_000 + time.1 + 10_000_000;
            redis::cmd("ZADD")
                .arg(&key)
                .arg(future)
                .arg("seed")
                .query_async::<usize>(&mut f.connection)
                .await
                .unwrap();
            let results = join_all((0..128).map(|_| f.store.is_limited("shared", &config))).await;
            assert_eq!(
                results
                    .into_iter()
                    .filter(|limited| !limited.as_ref().unwrap())
                    .count(),
                31
            );
            assert_eq!(
                redis::cmd("ZCARD")
                    .arg(&key)
                    .query_async::<usize>(&mut f.connection)
                    .await
                    .unwrap(),
                32
            );
            let scores: Vec<(String, u64)> = redis::cmd("ZRANGE")
                .arg(&key)
                .arg(0)
                .arg(-1)
                .arg("WITHSCORES")
                .query_async(&mut f.connection)
                .await
                .unwrap();
            assert!(scores.iter().all(|(_, score)| *score == future));
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn clients_rules_settings_and_prefixes_are_isolated() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let config = RateLimitConfig::default().max_requests(1);
            assert!(!f.store.is_limited("a:b", &config).await.unwrap());
            assert!(f.store.is_limited("a:b", &config).await.unwrap());
            assert!(!f.store.is_limited("other", &config).await.unwrap());
            assert!(
                !f.store
                    .is_limited("a:b", &config.clone().namespace("login:世界"))
                    .await
                    .unwrap()
            );
            assert!(
                !f.store
                    .is_limited("a:b", &config.clone().window_secs(2))
                    .await
                    .unwrap()
            );
            assert!(
                !f.store
                    .is_limited("a:b", &config.clone().max_requests(2))
                    .await
                    .unwrap()
            );
            let second = f.store.clone().with_prefix(&format!("{}other:", f.prefix));
            assert!(!second.is_limited("a:b", &config).await.unwrap());
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn ttl_expires_and_denied_requests_do_not_renew_it() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let mut config = RateLimitConfig::default().max_requests(1);
            config.window_secs = Duration::from_millis(1_000);
            let key = f.key("a", &config);
            assert!(!f.store.is_limited("a", &config).await.unwrap());
            let before: i64 = redis::cmd("PTTL")
                .arg(&key)
                .query_async(&mut f.connection)
                .await
                .unwrap();
            assert!((1..=1_000).contains(&before));
            actix_web::rt::time::sleep(Duration::from_millis(30)).await;
            assert!(f.store.is_limited("a", &config).await.unwrap());
            let after: i64 = redis::cmd("PTTL")
                .arg(&key)
                .query_async(&mut f.connection)
                .await
                .unwrap();
            assert!(after < before);
            actix_web::rt::time::sleep(Duration::from_millis(1_050)).await;
            assert_eq!(
                redis::cmd("EXISTS")
                    .arg(&key)
                    .query_async::<usize>(&mut f.connection)
                    .await
                    .unwrap(),
                0
            );
            assert!(!f.store.is_limited("a", &config).await.unwrap());
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn backend_error_policies_and_recovery() {
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        with_fixture(policy, move |f| {
            Box::pin(async move {
                let config = RateLimitConfig::default().max_requests(1);
                let key = f.key("a", &config);
                redis::cmd("SET")
                    .arg(&key)
                    .arg("wrong-type")
                    .arg("PX")
                    .arg(5_000)
                    .query_async::<()>(&mut f.connection)
                    .await
                    .unwrap();
                let result = f.store.is_limited("a", &config).await;
                match policy {
                    FailurePolicy::FailOpen => assert!(!result.unwrap()),
                    FailurePolicy::FailClosed => assert!(result.is_err()),
                }
                redis::cmd("DEL")
                    .arg(&key)
                    .query_async::<usize>(&mut f.connection)
                    .await
                    .unwrap();
                assert!(!f.store.is_limited("a", &config).await.unwrap());
                assert!(f.store.is_limited("a", &config).await.unwrap());
                assert!(
                    f.store
                        .is_limited("a", &config.clone().max_requests(0))
                        .await
                        .unwrap()
                );
                assert!(
                    f.store
                        .is_limited("a", &config.clone().window_secs(0))
                        .await
                        .is_err()
                );
            })
        })
        .await;
    }
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn lua_exact_boundary_and_same_microsecond_regression() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            // Replace only the clock read; exercise the actual admission script with deterministic time.
            let source = include_str!("../src/store/sliding_window.lua").replace(
                "local time = redis.call(\"TIME\")",
                "local time = { ARGV[3], ARGV[4] }",
            );
            let script = redis::Script::new(&source);
            let key = format!("{}clock", f.prefix);
            for (micros, expected) in [
                (0, false),
                (0, false),
                (0, true),
                (999_999, true),
                (1_000_000, false),
                (1_500_000, false),
                (1_999_999, true),
                (2_000_000, false),
            ] {
                let limited: bool = script
                    .key(&key)
                    .arg(1_000_000)
                    .arg(2)
                    .arg(100)
                    .arg(micros)
                    .invoke_async(&mut f.connection)
                    .await
                    .unwrap();
                assert_eq!(limited, expected);
            }
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn generated_workloads_match_memory_and_an_independent_quota_model() {
    use actix_web_ratelimit::store::MemoryStore;
    use std::collections::HashMap;
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let memory = MemoryStore::new();
            let mut model = HashMap::new();
            let clients = ["", "a:b", "{scope}", "*?[id]", "世界", "binary\0id"];
            let namespaces = ["", "a", "a:b", "世界:login"];
            let mut seed = 109_u64;
            for _ in 0..2_000 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let n = (seed >> 32) as usize;
                let client = clients[n % clients.len()];
                let namespace = namespaces[(n / 6) % namespaces.len()];
                let limit = [0, 1, 7][(n / 24) % 3];
                let window = [60, 120][(n / 72) % 2];
                let config = RateLimitConfig::default()
                    .namespace(namespace)
                    .max_requests(limit)
                    .window_secs(window);
                let count = model.entry((client, namespace, limit, window)).or_insert(0);
                let expected = *count >= limit;
                if !expected {
                    *count += 1;
                }
                assert_eq!(memory.is_limited(client, &config).await.unwrap(), expected);
                assert_eq!(f.store.is_limited(client, &config).await.unwrap(), expected);
            }
        })
    })
    .await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn redis_precision_and_maximum_numeric_configuration_are_safe() {
    with_fixture(FailurePolicy::FailClosed, move |f| {
        Box::pin(async move {
            let mut config = RateLimitConfig::default().max_requests(1);
            config.window_secs = Duration::from_micros((1 << 53) - 1);
            assert!(!f.store.is_limited("long-window", &config).await.unwrap());
            assert!(f.store.is_limited("long-window", &config).await.unwrap());
            let ttl: i64 = redis::cmd("PTTL")
                .arg(f.key("long-window", &config))
                .query_async(&mut f.connection)
                .await
                .unwrap();
            assert!((9_007_199_254_000..=9_007_199_254_741).contains(&ttl));
            if usize::BITS >= 64 {
                config.max_requests = ((1_u64 << 53) - 1) as usize;
                assert!(!f.store.is_limited("large-quota", &config).await.unwrap());
            }
            config.window_secs = Duration::from_nanos(1);
            assert!(!f.store.is_limited("tiny-window", &config).await.unwrap());
            // The microsecond window must not become a zero-duration Redis expiry.
            let ttl: i64 = redis::cmd("PTTL")
                .arg(f.key("tiny-window", &config))
                .query_async(&mut f.connection)
                .await
                .unwrap();
            assert!([-2, 0, 1].contains(&ttl));
        })
    })
    .await;
}
