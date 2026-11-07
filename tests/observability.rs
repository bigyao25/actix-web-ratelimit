use actix_web::{
    App, HttpResponse,
    dev::{Service, ServiceRequest, Transform},
    http::StatusCode,
    test, web,
};
use actix_web_ratelimit::{
    RateLimit,
    config::RateLimitConfig,
    observability::{
        ObservedRateLimitDecision, RateLimitErrorKind, RateLimitEvent, RateLimitObserver,
        RateLimitOutcome,
    },
    store::{MemoryStore, RateLimitStore, StoreError},
};
use futures_util::future::BoxFuture;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug, Clone)]
struct Snapshot {
    outcome: RateLimitOutcome,
    duration: Duration,
    metadata: Option<actix_web_ratelimit::store::RateLimitMetadata>,
    error: Option<RateLimitErrorKind>,
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Snapshot>>);
impl RateLimitObserver for Recorder {
    fn on_event(&self, event: &RateLimitEvent<'_>) {
        assert_eq!(event.namespace, "test");
        assert_eq!(event.window, Duration::from_secs(3600));
        self.0.lock().unwrap().push(Snapshot {
            outcome: event.outcome,
            duration: event.check_duration,
            metadata: event.metadata,
            error: event.error_kind,
        });
    }
}

fn config(quota: usize) -> RateLimitConfig {
    RateLimitConfig::default()
        .namespace("test")
        .window_secs(3600)
        .max_requests(quota)
}

#[actix_web::test]
async fn observation_and_headers_are_independent_and_preserve_quota() {
    for headers in [false, true] {
        for observed in [false, true] {
            for quota in [0, 2] {
                let recorder = Arc::new(Recorder::default());
                let mut middleware =
                    RateLimit::new(config(quota), MemoryStore::new()).response_headers(headers);
                if observed {
                    middleware = middleware.observe(recorder.clone());
                }
                let app = test::init_service(
                    App::new()
                        .wrap(middleware)
                        .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
                )
                .await;
                for i in 0..4 {
                    let response =
                        test::call_service(&app, test::TestRequest::get().to_request()).await;
                    let allowed = i < quota;
                    assert_eq!(
                        response.status(),
                        if allowed {
                            StatusCode::OK
                        } else {
                            StatusCode::TOO_MANY_REQUESTS
                        }
                    );
                    assert_eq!(response.headers().contains_key("ratelimit"), headers);
                    assert_eq!(
                        response.headers().contains_key("retry-after"),
                        headers && !allowed && quota != 0
                    );
                }
                let events = recorder.0.lock().unwrap();
                assert_eq!(events.len(), if observed { 4 } else { 0 });
                for (i, event) in events.iter().enumerate() {
                    assert_eq!(
                        event.outcome,
                        if i < quota {
                            RateLimitOutcome::Allowed
                        } else {
                            RateLimitOutcome::Limited
                        }
                    );
                    assert_eq!(event.metadata.is_some(), headers);
                    if let Some(metadata) = event.metadata {
                        assert_eq!(metadata.remaining, quota.saturating_sub(i + 1));
                    }
                    assert_eq!(event.error, None);
                }
            }
        }
    }
}

struct LegacyStore {
    calls: Arc<AtomicUsize>,
    fail: bool,
}
impl RateLimitStore for LegacyStore {
    fn is_limited<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            actix_web::rt::time::sleep(Duration::from_millis(10)).await;
            if self.fail {
                Err(StoreError::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "private detail",
                )))
            } else {
                Ok(false)
            }
        })
    }
}

#[actix_web::test]
async fn legacy_stores_execute_once_and_metadata_absence_is_not_fail_open() {
    for headers in [false, true] {
        for fail in [false, true] {
            let recorder = Arc::new(Recorder::default());
            let calls = Arc::new(AtomicUsize::new(0));
            let store: Box<dyn RateLimitStore> = Box::new(LegacyStore {
                calls: calls.clone(),
                fail,
            });
            let app = test::init_service(
                App::new()
                    .wrap(
                        RateLimit::new(config(10), Arc::new(store))
                            .response_headers(headers)
                            .observe(recorder.clone()),
                    )
                    .default_service(web::to(|| async { HttpResponse::BadRequest().finish() })),
            )
            .await;
            let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
            assert_eq!(
                response.status(),
                if fail {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::BAD_REQUEST
                }
            );
            assert!(!response.headers().contains_key("ratelimit"));
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            let events = recorder.0.lock().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].outcome,
                if fail {
                    RateLimitOutcome::StoreError
                } else {
                    RateLimitOutcome::Allowed
                }
            );
            assert_eq!(events[0].error, fail.then_some(RateLimitErrorKind::Timeout));
            assert!(events[0].duration >= Duration::from_millis(10));
            assert_eq!(events[0].metadata, None);
        }
    }
}

struct ObservedOnly(AtomicUsize);
impl RateLimitStore for ObservedOnly {
    fn is_limited<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        panic!("must forward observed override")
    }
    fn check_observed<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
        _: bool,
    ) -> BoxFuture<'a, Result<ObservedRateLimitDecision, StoreError>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(ObservedRateLimitDecision {
                decision: actix_web_ratelimit::store::RateLimitDecision {
                    allowed: true,
                    metadata: None,
                },
                fail_open: true,
                error_kind: Some(RateLimitErrorKind::Connection),
            })
        })
    }
}

#[actix_web::test]
async fn arc_and_box_forward_observed_override_without_losing_fail_open() {
    let store = Arc::new(ObservedOnly(AtomicUsize::new(0)));
    let boxed: Box<dyn RateLimitStore> = Box::new(store.clone());
    let recorder = Arc::new(Recorder::default());
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config(10), Arc::new(boxed)).observe(recorder.clone()))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    assert_eq!(
        test::call_service(&app, test::TestRequest::get().to_request())
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(store.0.load(Ordering::Relaxed), 1);
    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, RateLimitOutcome::FailOpen);
    assert_eq!(events[0].error, Some(RateLimitErrorKind::Connection));
}

#[actix_web::test]
async fn disabled_observation_does_not_call_extension() {
    struct BasicOnly;
    impl RateLimitStore for BasicOnly {
        fn is_limited<'a>(
            &'a self,
            _: &'a str,
            _: &'a RateLimitConfig,
        ) -> BoxFuture<'a, Result<bool, StoreError>> {
            Box::pin(async { Ok(false) })
        }
        fn check_observed<'a>(
            &'a self,
            _: &'a str,
            _: &'a RateLimitConfig,
            _: bool,
        ) -> BoxFuture<'a, Result<ObservedRateLimitDecision, StoreError>> {
            panic!("observation disabled")
        }
    }
    for headers in [false, true] {
        let app = test::init_service(
            App::new()
                .wrap(RateLimit::new(config(1), BasicOnly).response_headers(headers))
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        assert_eq!(
            test::call_service(&app, test::TestRequest::get().to_request())
                .await
                .status(),
            StatusCode::OK
        );
    }
}

#[actix_web::test]
async fn callback_precedes_handler_and_does_not_classify_downstream_errors() {
    let recorder = Arc::new(Recorder::default());
    let handler_recorder = recorder.clone();
    let middleware = RateLimit::new(config(10), MemoryStore::new())
        .observe(recorder.clone())
        .new_transform(actix_service::fn_service(move |_: ServiceRequest| {
            assert_eq!(handler_recorder.0.lock().unwrap().len(), 1);
            async {
                actix_web::rt::time::sleep(Duration::from_millis(50)).await;
                Err::<actix_web::dev::ServiceResponse, _>(
                    actix_web::error::ErrorInternalServerError("handler"),
                )
            }
        }))
        .await
        .unwrap();
    assert!(
        middleware
            .call(test::TestRequest::default().to_srv_request())
            .await
            .is_err()
    );
    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, RateLimitOutcome::Allowed);
    // The event was already recorded when the handler started sleeping.
}

#[actix_web::test]
async fn concurrent_workers_share_observer_and_quota() {
    let recorder = Arc::new(Recorder::default());
    let store = Arc::new(MemoryStore::new());
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            let recorder = recorder.clone();
            std::thread::spawn(move || {
                actix_web::rt::System::new().block_on(async move {
                    let app = test::init_service(
                        App::new()
                            .wrap(RateLimit::new(config(31), store).observe(recorder))
                            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
                    )
                    .await;
                    let results =
                        futures_util::future::join_all((0..50).map(|_| {
                            test::call_service(&app, test::TestRequest::get().to_request())
                        }))
                        .await;
                    results
                        .iter()
                        .filter(|r| r.status() == StatusCode::OK)
                        .count()
                })
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .sum::<usize>(),
        31
    );
    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 200);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.outcome == RateLimitOutcome::Allowed)
            .count(),
        31
    );
}

#[actix_web::test]
async fn callback_runs_after_memory_locks_release() {
    let store = Arc::new(MemoryStore::new());
    let callback_store = store.clone();
    let app = test::init_service(
        App::new()
            .wrap(
                RateLimit::new(config(1), store).observe(move |_: &RateLimitEvent<'_>| {
                    // Re-entering cleanup needs exclusive shard access, deadlocking if a
                    // bucket guard remains held by the check.
                    callback_store.prune_expired();
                }),
            )
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    assert_eq!(
        test::call_service(&app, test::TestRequest::get().to_request())
            .await
            .status(),
        StatusCode::OK
    );
}

#[actix_web::test]
async fn callback_panic_preserves_admission_and_rejection() {
    for headers in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let app = test::init_service(
            App::new()
                .wrap(
                    RateLimit::new(config(1), MemoryStore::new())
                        .response_headers(headers)
                        .observe(move |_: &RateLimitEvent<'_>| {
                            callback_calls.fetch_add(1, Ordering::Relaxed);
                            panic!("observer");
                        }),
                )
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
            assert_eq!(response.status(), status);
            assert_eq!(response.headers().contains_key("ratelimit"), headers);
        }
        // The first panic disables subsequent callbacks for this registration.
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}

struct PanicPayload {
    drops: Arc<AtomicUsize>,
    panic_on_drop: bool,
}

impl Drop for PanicPayload {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
        if self.panic_on_drop {
            panic!("payload destructor");
        }
    }
}

#[actix_web::test]
async fn panicking_payloads_are_dropped_and_observer_is_disabled() {
    for panic_on_drop in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let payload_drops = drops.clone();
        let app = test::init_service(
            App::new()
                .wrap(RateLimit::new(config(1), MemoryStore::new()).observe(
                    move |_: &RateLimitEvent<'_>| {
                        callback_calls.fetch_add(1, Ordering::Relaxed);
                        std::panic::panic_any(PanicPayload {
                            drops: payload_drops.clone(),
                            panic_on_drop,
                        });
                    },
                ))
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
            assert_eq!(response.status(), status);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}

#[actix_web::test]
async fn callback_panic_preserves_store_error() {
    let app = test::init_service(
        App::new()
            .wrap(
                RateLimit::new(
                    config(1),
                    LegacyStore {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail: true,
                    },
                )
                .observe(|_: &RateLimitEvent<'_>| panic!("observer")),
            )
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
