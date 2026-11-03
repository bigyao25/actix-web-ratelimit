#![cfg(feature = "redis")]
use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{FailurePolicy, RateLimitStore, RedisStore, RedisStoreOptions},
};
use futures_util::future::join_all;
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
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(format!("{}*", self.prefix))
            .query_async(&mut self.connection)
            .await
            .unwrap();
        if !keys.is_empty() {
            redis::cmd("DEL")
                .arg(keys)
                .query_async::<usize>(&mut self.connection)
                .await
                .unwrap();
        }
    }
}

#[actix_web::test]
#[ignore = "requires Redis; run cargo test --test redis -- --ignored"]
async fn burst_admits_exactly_the_configured_limit() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
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
    f.cleanup().await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn independent_connections_share_one_atomic_quota() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
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
    f.cleanup().await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn same_timestamp_members_are_unique_even_when_server_clock_moves_backwards() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
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
    f.cleanup().await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn clients_rules_settings_and_prefixes_are_isolated() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
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
    f.cleanup().await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn ttl_expires_and_denied_requests_do_not_renew_it() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
    let mut config = RateLimitConfig::default().max_requests(1);
    config.window_secs = Duration::from_millis(200);
    let key = f.key("a", &config);
    assert!(!f.store.is_limited("a", &config).await.unwrap());
    let before: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut f.connection)
        .await
        .unwrap();
    assert!((1..=200).contains(&before));
    actix_web::rt::time::sleep(Duration::from_millis(30)).await;
    assert!(f.store.is_limited("a", &config).await.unwrap());
    let after: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut f.connection)
        .await
        .unwrap();
    assert!(after < before);
    actix_web::rt::time::sleep(Duration::from_millis(220)).await;
    assert_eq!(
        redis::cmd("EXISTS")
            .arg(&key)
            .query_async::<usize>(&mut f.connection)
            .await
            .unwrap(),
        0
    );
    assert!(!f.store.is_limited("a", &config).await.unwrap());
    f.cleanup().await;
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn backend_error_policies_and_recovery() {
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        let mut f = Fixture::new(policy).await;
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
        f.cleanup().await;
    }
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn lua_exact_boundary_and_same_microsecond_regression() {
    let mut f = Fixture::new(FailurePolicy::FailClosed).await;
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
    f.cleanup().await;
}
