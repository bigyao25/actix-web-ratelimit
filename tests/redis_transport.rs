#![cfg(feature = "redis")]
use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{FailurePolicy, RateLimitStore, RedisStore, RedisStoreOptions},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::{JoinHandle, JoinSet},
};

struct Proxy {
    url: String,
    task: JoinHandle<()>,
    connections: Arc<AtomicUsize>,
    stall: Arc<Notify>,
    stalled: Arc<Notify>,
    disconnect: Arc<Notify>,
    disconnected: Arc<Notify>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    async fn start() -> Self {
        let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
        let info = redis::Client::open(url.as_str())
            .unwrap()
            .get_connection_info()
            .clone();
        let redis::ConnectionAddr::Tcp(host, port) = info.addr().clone() else {
            panic!("transport tests require a redis:// TCP URL")
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_, rest) = url.split_once("://").unwrap();
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let auth = authority
            .rsplit_once('@')
            .map(|(auth, _)| format!("{auth}@"))
            .unwrap_or_default();
        let url = format!("redis://{auth}{address}/{path}");
        let connections = Arc::new(AtomicUsize::new(0));
        let stall = Arc::new(Notify::new());
        let stalled = Arc::new(Notify::new());
        let disconnect = Arc::new(Notify::new());
        let disconnected = Arc::new(Notify::new());
        let task = tokio::spawn({
            let connections = connections.clone();
            let stall = stall.clone();
            let stalled = stalled.clone();
            let disconnect = disconnect.clone();
            let disconnected = disconnected.clone();
            async move {
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (mut downstream, _) = accepted.unwrap();
                            connections.fetch_add(1, Ordering::SeqCst);
                            let mut upstream = TcpStream::connect((host.as_str(), port)).await.unwrap();
                            let (stall, stalled, disconnect, disconnected) = (stall.clone(), stalled.clone(), disconnect.clone(), disconnected.clone());
                            tasks.spawn(async move {
                                tokio::select! {
                                    _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => {},
                                    _ = stall.notified() => {
                                        stalled.notify_one();
                                        std::future::pending::<()>().await;
                                    },
                                    _ = disconnect.notified() => { disconnected.notify_one(); },
                                }
                            });
                        },
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                    }
                }
            }
        });
        Self {
            url,
            task,
            connections,
            stall,
            stalled,
            disconnect,
            disconnected,
        }
    }
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn checks_reuse_one_connection_and_reconnect_after_transport_loss() {
    let proxy = Proxy::start().await;
    let store = RedisStore::with_options(
        &proxy.url,
        RedisStoreOptions {
            timeout: Duration::from_millis(200),
            failure_policy: FailurePolicy::FailClosed,
        },
    )
    .await
    .unwrap()
    .with_prefix(&format!(
        "ratelimit:transport:{}:{}:",
        std::process::id(),
        proxy.url
    ));
    let config = RateLimitConfig::default().max_requests(1).window_secs(10);
    assert!(!store.is_limited("a", &config).await.unwrap());
    let results =
        futures_util::future::join_all((0..128).map(|_| store.is_limited("a", &config))).await;
    assert!(results.into_iter().all(|r| r.unwrap()));
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 1);
    proxy.disconnect.notify_one();
    proxy.disconnected.notified().await;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(limited) = store.is_limited("a", &config).await {
                break limited;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(result);
    assert!(proxy.connections.load(Ordering::SeqCst) >= 2);
}

#[actix_web::test]
#[ignore = "requires Redis"]
async fn stalled_redis_obeys_deadlines_without_blocking_the_runtime() {
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        let proxy = Proxy::start().await;
        let store = RedisStore::with_options(
            &proxy.url,
            RedisStoreOptions {
                timeout: Duration::from_millis(100),
                failure_policy: policy,
            },
        )
        .await
        .unwrap();
        proxy.stall.notify_one();
        proxy.stalled.notified().await;
        let start = Instant::now();
        let config = RateLimitConfig::default();
        let (result, tick_elapsed) = tokio::join!(store.is_limited("a", &config), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            start.elapsed()
        });
        assert!(
            tick_elapsed < Duration::from_millis(80),
            "Redis blocked another task: {tick_elapsed:?}"
        );
        assert!(start.elapsed() < Duration::from_millis(500));
        match policy {
            FailurePolicy::FailOpen => assert!(!result.unwrap()),
            FailurePolicy::FailClosed => assert!(result.is_err()),
        }
    }
}

#[actix_web::test]
async fn initialization_is_bounded_and_rejects_invalid_options() {
    assert!(RedisStore::new("invalid://host").await.is_err());
    assert!(
        RedisStore::with_options(
            "redis://127.0.0.1/",
            RedisStoreOptions {
                timeout: Duration::ZERO,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("redis://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_connection, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let start = Instant::now();
    let result = RedisStore::with_options(
        &url,
        RedisStoreOptions {
            timeout: Duration::from_millis(50),
            ..Default::default()
        },
    )
    .await;
    server.abort();
    let _ = server.await;
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
}
