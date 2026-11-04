#![cfg(feature = "redis")]
use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{FailurePolicy, RateLimitStore, RedisStore, RedisStoreOptions},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};

#[derive(Clone, Copy)]
enum Scenario {
    Healthy,
    RejectPing,
    RejectScript,
    NoScriptOnce,
    InvalidResult,
    DelayedResult,
    DropFirstCheck,
}
#[derive(Default)]
struct Counters {
    connections: AtomicUsize,
    loads: AtomicUsize,
    checks: AtomicUsize,
}
struct Server {
    url: String,
    task: JoinHandle<()>,
    counters: Arc<Counters>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn command(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> io::Result<Vec<Vec<u8>>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let count = line
        .strip_prefix('*')
        .and_then(|s| s.trim().parse::<usize>().ok())
        .ok_or_else(|| io::Error::other("expected RESP array"))?;
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        line.clear();
        reader.read_line(&mut line).await?;
        let len = line
            .strip_prefix('$')
            .and_then(|s| s.trim().parse::<usize>().ok())
            .ok_or_else(|| io::Error::other("expected RESP bulk string"))?;
        let mut bytes = vec![0; len + 2];
        reader.read_exact(&mut bytes).await?;
        bytes.truncate(len);
        args.push(bytes);
    }
    Ok(args)
}

impl Server {
    async fn start(scenario: Scenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}/", listener.local_addr().unwrap());
        let counters = Arc::new(Counters::default());
        let stats = counters.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        stats.connections.fetch_add(1, Ordering::SeqCst);
                        let stats = stats.clone();
                        tasks.spawn(async move {
                            let (read, mut write) = stream.into_split();
                            let mut read = BufReader::new(read);
                            while let Ok(args) = command(&mut read).await {
                                let digest_response = if args[0] == b"SCRIPT" {
                                    format!("$40\r\n{}\r\n", redis::Script::new(std::str::from_utf8(&args[2]).unwrap()).get_hash())
                                } else { String::new() };
                                let reply: &[u8] = match args[0].as_slice() {
                                    b"CLIENT" => b"+OK\r\n",
                                    b"PING" if matches!(scenario, Scenario::RejectPing) => b"-ERR health check failed\r\n",
                                    b"PING" => b"+PONG\r\n",
                                    b"SCRIPT" => {
                                        stats.loads.fetch_add(1, Ordering::SeqCst);
                                        if matches!(scenario, Scenario::RejectScript) { b"-NOPERM script loading forbidden\r\n" }
                                        else { digest_response.as_bytes() }
                                    },
                                    b"EVALSHA" => {
                                        let check = stats.checks.fetch_add(1, Ordering::SeqCst);
                                        match scenario {
                                            Scenario::NoScriptOnce if check == 0 => b"-NOSCRIPT cache entry missing\r\n",
                                            Scenario::InvalidResult => b"$4\r\noops\r\n",
                                            Scenario::DelayedResult if check == 0 => {
                                                tokio::time::sleep(Duration::from_millis(150)).await;
                                                b":1\r\n"
                                            },
                                            Scenario::DropFirstCheck if check == 0 => break,
                                            _ => b":0\r\n",
                                        }
                                    },
                                    _ => b"-ERR unexpected command\r\n",
                                };
                                if write.write_all(reply).await.is_err() { break; }
                            }
                        });
                    },
                    _ = tasks.join_next(), if !tasks.is_empty() => {},
                }
            }
        });
        Self {
            url,
            task,
            counters,
        }
    }

    async fn store(&self, policy: FailurePolicy, timeout: Duration) -> RedisStore {
        RedisStore::with_options(
            &self.url,
            RedisStoreOptions {
                failure_policy: policy,
                timeout,
            },
        )
        .await
        .unwrap()
    }
}

#[actix_web::test]
async fn initialization_propagates_ping_and_script_permission_errors() {
    for scenario in [Scenario::RejectPing, Scenario::RejectScript] {
        let server = Server::start(scenario).await;
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let result = RedisStore::with_options(
                &server.url,
                RedisStoreOptions {
                    failure_policy: policy,
                    ..Default::default()
                },
            )
            .await;
            assert!(result.is_err());
        }
    }
}

#[actix_web::test]
async fn missing_cached_script_is_reloaded_and_admission_retried() {
    let server = Server::start(Scenario::NoScriptOnce).await;
    let store = server
        .store(FailurePolicy::FailClosed, Duration::from_secs(1))
        .await;
    assert!(
        !store
            .is_limited("a", &RateLimitConfig::default())
            .await
            .unwrap()
    );
    assert_eq!(server.counters.loads.load(Ordering::SeqCst), 2);
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 2);
    assert_eq!(server.counters.connections.load(Ordering::SeqCst), 1);
}

#[actix_web::test]
async fn malformed_admission_results_follow_the_configured_failure_policy() {
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        let server = Server::start(Scenario::InvalidResult).await;
        let store = server.store(policy, Duration::from_secs(1)).await;
        let result = store.is_limited("a", &RateLimitConfig::default()).await;
        match policy {
            FailurePolicy::FailOpen => assert!(!result.unwrap()),
            FailurePolicy::FailClosed => assert!(result.is_err()),
        }
    }
}

#[actix_web::test]
async fn cancelled_check_does_not_consume_the_next_checks_response() {
    let server = Server::start(Scenario::DelayedResult).await;
    let store = server
        .store(FailurePolicy::FailClosed, Duration::from_millis(50))
        .await;
    let config = RateLimitConfig::default();
    assert!(store.is_limited("a", &config).await.is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The timed-out response was limited=true; the next response must be false.
    assert!(!store.is_limited("a", &config).await.unwrap());
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 2);
}

#[actix_web::test]
async fn disconnected_checks_are_not_silently_replayed_as_new_admissions() {
    let server = Server::start(Scenario::DropFirstCheck).await;
    let store = server
        .store(FailurePolicy::FailClosed, Duration::from_secs(1))
        .await;
    let config = RateLimitConfig::default();
    assert!(store.is_limited("a", &config).await.is_err());
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 1);
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
    assert!(!result);
    assert!(server.counters.connections.load(Ordering::SeqCst) >= 2);
}

#[actix_web::test]
async fn zero_quotas_and_invalid_configurations_never_send_admission_commands() {
    let server = Server::start(Scenario::Healthy).await;
    let store = server
        .store(FailurePolicy::FailOpen, Duration::from_secs(1))
        .await;
    assert!(
        store
            .is_limited("a", &RateLimitConfig::default().max_requests(0))
            .await
            .unwrap()
    );
    assert!(
        store
            .is_limited("a", &RateLimitConfig::default().window_secs(0))
            .await
            .is_err()
    );
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 0);
}
