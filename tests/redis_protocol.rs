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
    Limited,
    BackendError,
    RejectPing,
    RejectScript,
    NoScriptOnce,
    InvalidResult,
    Metadata(&'static [u8]),
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
                                            Scenario::Limited if args.last().is_some_and(|arg| arg == b"details") => b"*3\r\n:1\r\n:0\r\n:750000\r\n",
                                            Scenario::Limited => b":1\r\n",
                                            Scenario::BackendError => b"-ERR private backend message\r\n",
                                            Scenario::NoScriptOnce if check == 0 => b"-NOSCRIPT cache entry missing\r\n",
                                            Scenario::InvalidResult => b"$4\r\noops\r\n",
                                            Scenario::Metadata(reply) => reply,
                                            Scenario::DelayedResult if check == 0 => {
                                                tokio::time::sleep(Duration::from_millis(150)).await;
                                                b":1\r\n"
                                            },
                                            Scenario::DropFirstCheck if check == 0 => break,
                                            _ if args.last().is_some_and(|arg| arg == b"details") => b"*3\r\n:0\r\n:9\r\n:0\r\n",
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
    let decision = store
        .check("a", &RateLimitConfig::default().max_requests(0))
        .await
        .unwrap();
    assert!(!decision.allowed);
    assert_eq!(decision.metadata.unwrap().remaining, 0);
    assert_eq!(decision.metadata.unwrap().retry_after, None);
    assert!(
        store
            .check("a", &RateLimitConfig::default().window_secs(0))
            .await
            .is_err()
    );
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 0);
}

#[actix_web::test]
async fn detailed_checks_use_one_command_and_recover_from_noscript() {
    for scenario in [Scenario::Healthy, Scenario::NoScriptOnce] {
        let server = Server::start(scenario).await;
        let store = server
            .store(FailurePolicy::FailClosed, Duration::from_secs(1))
            .await;
        let decision = store.check("a", &RateLimitConfig::default()).await.unwrap();
        assert!(decision.allowed);
        assert_eq!(decision.metadata.unwrap().remaining, 9);
        assert_eq!(decision.metadata.unwrap().retry_after, None);
        let expected = if matches!(scenario, Scenario::NoScriptOnce) {
            2
        } else {
            1
        };
        assert_eq!(server.counters.checks.load(Ordering::SeqCst), expected);
        assert_eq!(server.counters.loads.load(Ordering::SeqCst), expected);
    }
}

#[actix_web::test]
async fn detailed_rejections_decode_microsecond_delays() {
    let server = Server::start(Scenario::Metadata(b"*3\r\n:1\r\n:0\r\n:750000\r\n")).await;
    let store = server
        .store(FailurePolicy::FailClosed, Duration::from_secs(1))
        .await;
    let decision = store.check("a", &RateLimitConfig::default()).await.unwrap();
    assert!(!decision.allowed);
    assert_eq!(decision.metadata.unwrap().remaining, 0);
    assert_eq!(
        decision.metadata.unwrap().retry_after,
        Some(Duration::from_millis(750))
    );
}

#[actix_web::test]
async fn malformed_metadata_follows_failure_policy_without_fabricating_quota() {
    for reply in [
        b":0\r\n".as_slice(),
        b"*2\r\n:0\r\n:9\r\n",
        b"*3\r\n:0\r\n:10\r\n:0\r\n",
        b"*3\r\n:0\r\n:-1\r\n:0\r\n",
        b"*3\r\n:0\r\n:9\r\n:1\r\n",
        b"*3\r\n:1\r\n:1\r\n:1\r\n",
        b"*3\r\n:1\r\n:0\r\n:0\r\n",
        b"*3\r\n:2\r\n:0\r\n:1\r\n",
    ] {
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let server = Server::start(Scenario::Metadata(reply)).await;
            let store = server.store(policy, Duration::from_secs(1)).await;
            let result = store.check("a", &RateLimitConfig::default()).await;
            match policy {
                FailurePolicy::FailOpen => {
                    let decision = result.unwrap();
                    assert!(decision.allowed);
                    assert_eq!(decision.metadata, None);
                }
                FailurePolicy::FailClosed => assert!(result.is_err()),
            }
        }
    }
}

#[actix_web::test]
async fn detailed_timeouts_keep_fail_open_and_fail_closed_semantics() {
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        let server = Server::start(Scenario::DelayedResult).await;
        let store = server.store(policy, Duration::from_millis(50)).await;
        let result = store.check("a", &RateLimitConfig::default()).await;
        match policy {
            FailurePolicy::FailOpen => {
                let decision = result.unwrap();
                assert!(decision.allowed);
                assert_eq!(decision.metadata, None);
            }
            FailurePolicy::FailClosed => assert!(result.is_err()),
        }
    }
}

#[actix_web::test]
async fn observed_redis_paths_report_real_outcomes_without_extra_commands() {
    use actix_web::{App, HttpResponse, http::StatusCode, test, web};
    use actix_web_ratelimit::{
        RateLimit,
        observability::{RateLimitErrorKind, RateLimitEvent, RateLimitOutcome},
    };
    use std::sync::Mutex;

    for headers in [false, true] {
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            for scenario in [
                Scenario::Healthy,
                Scenario::Limited,
                Scenario::BackendError,
                Scenario::InvalidResult,
                Scenario::DelayedResult,
                Scenario::DropFirstCheck,
                Scenario::NoScriptOnce,
            ] {
                let server = Server::start(scenario).await;
                let store = server.store(policy, Duration::from_millis(50)).await;
                // Exercise both forwarding implementations around a real RedisStore.
                let store: Arc<Box<dyn RateLimitStore>> = Arc::new(Box::new(store));
                let events = Arc::new(Mutex::new(Vec::new()));
                let callback_events = events.clone();
                let app = test::init_service(
                    App::new()
                        .wrap(
                            RateLimit::new(RateLimitConfig::default(), store)
                                .response_headers(headers)
                                .observe(move |event: &RateLimitEvent<'_>| {
                                    callback_events.lock().unwrap().push((
                                        event.outcome,
                                        event.error_kind,
                                        event.metadata,
                                    ));
                                }),
                        )
                        .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
                )
                .await;
                let response =
                    test::call_service(&app, test::TestRequest::get().to_request()).await;
                let failure = matches!(
                    scenario,
                    Scenario::BackendError
                        | Scenario::InvalidResult
                        | Scenario::DelayedResult
                        | Scenario::DropFirstCheck
                );
                let (outcome, status) = if failure {
                    match policy {
                        FailurePolicy::FailOpen => (RateLimitOutcome::FailOpen, StatusCode::OK),
                        FailurePolicy::FailClosed => (
                            RateLimitOutcome::StoreError,
                            StatusCode::SERVICE_UNAVAILABLE,
                        ),
                    }
                } else if matches!(scenario, Scenario::Limited) {
                    (RateLimitOutcome::Limited, StatusCode::TOO_MANY_REQUESTS)
                } else {
                    (RateLimitOutcome::Allowed, StatusCode::OK)
                };
                assert_eq!(response.status(), status);
                assert_eq!(
                    response.headers().contains_key("ratelimit"),
                    headers && !failure
                );
                let events = events.lock().unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].0, outcome);
                assert_eq!(events[0].2.is_some(), headers && !failure);
                let kind = match scenario {
                    Scenario::DelayedResult => Some(RateLimitErrorKind::Timeout),
                    Scenario::DropFirstCheck => Some(RateLimitErrorKind::Connection),
                    Scenario::BackendError | Scenario::InvalidResult => {
                        Some(RateLimitErrorKind::Backend)
                    }
                    _ => None,
                };
                assert_eq!(events[0].1, kind);
                let commands = if matches!(scenario, Scenario::NoScriptOnce) {
                    2
                } else {
                    1
                };
                assert_eq!(server.counters.checks.load(Ordering::SeqCst), commands);
                assert_eq!(server.counters.loads.load(Ordering::SeqCst), commands);
            }
        }
    }
}

#[actix_web::test]
async fn observed_zero_quota_and_invalid_config_do_not_contact_redis() {
    let server = Server::start(Scenario::Healthy).await;
    let store = server
        .store(FailurePolicy::FailOpen, Duration::from_secs(1))
        .await;
    for metadata in [false, true] {
        let observed = store
            .check_observed("a", &RateLimitConfig::default().max_requests(0), metadata)
            .await
            .unwrap();
        assert!(!observed.decision.allowed);
        assert!(!observed.fail_open);
        assert_eq!(observed.decision.metadata.is_some(), metadata);
        assert_eq!(observed.error_kind, None);
        assert!(
            store
                .check_observed("a", &RateLimitConfig::default().window_secs(0), metadata)
                .await
                .is_err()
        );
    }
    assert_eq!(server.counters.checks.load(Ordering::SeqCst), 0);
}
