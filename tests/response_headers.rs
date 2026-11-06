use actix_web::{App, HttpResponse, http::StatusCode, test, web};
use actix_web_ratelimit::{
    RateLimit,
    config::RateLimitConfig,
    store::{MemoryStore, RateLimitDecision, RateLimitMetadata, RateLimitStore, StoreError},
};
use futures_util::future::BoxFuture;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct PathStore {
    basic: Arc<AtomicUsize>,
    detailed: Arc<AtomicUsize>,
}

impl RateLimitStore for PathStore {
    fn is_limited<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.basic.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(false) })
    }

    fn check<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<RateLimitDecision, StoreError>> {
        self.detailed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(RateLimitDecision {
                allowed: true,
                metadata: Some(RateLimitMetadata {
                    remaining: 9,
                    retry_after: None,
                }),
            })
        })
    }
}

#[actix_web::test]
async fn opt_in_selects_exactly_one_store_path_and_wrappers_forward_metadata() {
    for enabled in [false, true] {
        let basic = Arc::new(AtomicUsize::new(0));
        let detailed = Arc::new(AtomicUsize::new(0));
        let store: Arc<dyn RateLimitStore> = Arc::new(PathStore {
            basic: basic.clone(),
            detailed: detailed.clone(),
        });
        let store: Box<dyn RateLimitStore> = Box::new(store);
        let middleware = RateLimit::new(RateLimitConfig::default(), store);
        // Exercise the constructor default without calling the builder on the disabled path.
        let middleware = if enabled {
            middleware.response_headers(true)
        } else {
            middleware
        };
        let app = test::init_service(
            App::new()
                .wrap(middleware)
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(basic.load(Ordering::SeqCst), usize::from(!enabled));
        assert_eq!(detailed.load(Ordering::SeqCst), usize::from(enabled));
        assert_eq!(response.headers().contains_key("ratelimit"), enabled);
        assert_eq!(response.headers().contains_key("ratelimit-policy"), enabled);
        assert!(!response.headers().contains_key("retry-after"));
    }
}

struct LegacyStore {
    calls: Arc<AtomicUsize>,
    limited: bool,
    fail: bool,
}

impl RateLimitStore for LegacyStore {
    fn is_limited<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(StoreError::new(std::io::Error::other("unavailable")))
            } else {
                Ok(self.limited)
            }
        })
    }
}

#[actix_web::test]
async fn legacy_stores_work_without_fabricated_metadata_and_errors_stay_503() {
    for (limited, fail, status) in [
        (false, false, StatusCode::OK),
        (true, false, StatusCode::TOO_MANY_REQUESTS),
        (false, true, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = test::init_service(
            App::new()
                .wrap(
                    RateLimit::new(
                        RateLimitConfig::default(),
                        LegacyStore {
                            calls: calls.clone(),
                            limited,
                            fail,
                        },
                    )
                    .response_headers(true),
                )
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), status);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for name in ["ratelimit", "ratelimit-policy", "retry-after"] {
            assert!(!response.headers().contains_key(name));
        }
    }
}

#[actix_web::test]
async fn memory_headers_count_once_and_preserve_callback_and_handler_responses() {
    let config = RateLimitConfig::default()
        .namespace("api")
        .max_requests(2)
        .window_secs(60)
        .exceeded(|_, _, _| {
            HttpResponse::TooManyRequests()
                .insert_header(("x-custom", "yes"))
                .body("limited")
        });
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()).response_headers(true))
            .default_service(web::to(|| async {
                HttpResponse::Created().body("accepted")
            })),
    )
    .await;
    for (status, remaining) in [
        (StatusCode::CREATED, 1),
        (StatusCode::CREATED, 0),
        (StatusCode::TOO_MANY_REQUESTS, 0),
    ] {
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get("ratelimit-policy").unwrap(),
            "\"api\";q=2;w=60"
        );
        assert_eq!(
            response
                .headers()
                .get("ratelimit")
                .unwrap()
                .to_str()
                .unwrap(),
            format!("\"api\";r={remaining}")
        );
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(response.headers().get("retry-after").unwrap(), "60");
            assert_eq!(response.headers().get("x-custom").unwrap(), "yes");
            assert_eq!(test::read_body(response).await, "limited");
        } else {
            assert!(!response.headers().contains_key("retry-after"));
            assert_eq!(test::read_body(response).await, "accepted");
        }
    }
}

#[actix_web::test]
async fn disabling_headers_after_enabling_restores_the_basic_path() {
    let app = test::init_service(
        App::new().wrap(
            RateLimit::new(
                RateLimitConfig::default().max_requests(0),
                MemoryStore::new(),
            )
            .response_headers(true)
            .response_headers(false),
        ),
    )
    .await;
    let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    for name in ["ratelimit", "ratelimit-policy", "retry-after"] {
        assert!(!response.headers().contains_key(name));
    }
}

#[actix_web::test]
async fn zero_quota_reports_zero_without_claiming_it_recovers() {
    let app = test::init_service(
        App::new().wrap(
            RateLimit::new(
                RateLimitConfig::default().max_requests(0),
                MemoryStore::new(),
            )
            .response_headers(true),
        ),
    )
    .await;
    let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers().get("ratelimit").unwrap(),
        "\"default\";r=0"
    );
    assert!(!response.headers().contains_key("retry-after"));
}

#[actix_web::test]
async fn existing_headers_from_handlers_and_callbacks_are_preserved() {
    let config = RateLimitConfig::default()
        .max_requests(1)
        .exceeded(|_, _, _| {
            HttpResponse::TooManyRequests()
                .insert_header(("ratelimit-policy", "\"custom\";q=1"))
                .insert_header(("ratelimit", "\"custom\";r=0"))
                .insert_header(("retry-after", "123"))
                .finish()
        });
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()).response_headers(true))
            .default_service(web::to(|| async {
                HttpResponse::Ok()
                    .insert_header(("ratelimit-policy", "\"custom\";q=1"))
                    .insert_header(("ratelimit", "\"custom\";r=0"))
                    .insert_header(("retry-after", "123"))
                    .finish()
            })),
    )
    .await;
    for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get("ratelimit-policy").unwrap(),
            "\"custom\";q=1"
        );
        assert_eq!(
            response.headers().get("ratelimit").unwrap(),
            "\"custom\";r=0"
        );
        assert_eq!(response.headers().get("retry-after").unwrap(), "123");
    }
}

#[actix_web::test]
async fn subsecond_windows_omit_w_and_retry_after_rounds_up() {
    let config = RateLimitConfig {
        window_secs: Duration::from_millis(750),
        max_requests: 1,
        ..Default::default()
    };
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()).response_headers(true))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get("ratelimit-policy").unwrap(),
            "\"default\";q=1"
        );
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        }
    }
}

#[actix_web::test]
async fn policy_names_are_structured_field_strings_and_invalid_names_do_not_affect_admission() {
    for (namespace, encoded) in [
        ("a\"b\\c", Some("\"a\\\"b\\\\c\";q=1;w=100")),
        ("世界", None),
        ("api\r\nx: y", None),
    ] {
        let app = test::init_service(
            App::new()
                .wrap(
                    RateLimit::new(
                        RateLimitConfig::default()
                            .namespace(namespace)
                            .max_requests(1),
                        MemoryStore::new(),
                    )
                    .response_headers(true),
                )
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
            assert_eq!(response.status(), status);
            assert_eq!(
                response
                    .headers()
                    .get("ratelimit-policy")
                    .map(|v| v.to_str().unwrap()),
                encoded
            );
            assert_eq!(
                response.headers().contains_key("ratelimit"),
                encoded.is_some()
            );
            assert_eq!(
                response.headers().contains_key("retry-after"),
                status == StatusCode::TOO_MANY_REQUESTS
            );
        }
    }
}

#[actix_web::test]
async fn oversized_structured_field_quotas_do_not_break_valid_store_configuration() {
    if usize::BITS < 64 {
        return;
    }
    let config = RateLimitConfig::default().max_requests(1_000_000_000_000_000_u64 as usize);
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()).response_headers(true))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("ratelimit-policy"));
    assert!(!response.headers().contains_key("ratelimit"));
}

#[actix_web::test]
async fn custom_non_429_rejections_do_not_receive_retry_after() {
    let config = RateLimitConfig::default()
        .max_requests(1)
        .exceeded(|_, _, _| HttpResponse::Forbidden().body("custom denial"));
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()).response_headers(true))
            .default_service(web::to(|| async {
                HttpResponse::TooManyRequests().finish()
            })),
    )
    .await;
    for status in [StatusCode::TOO_MANY_REQUESTS, StatusCode::FORBIDDEN] {
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(response.status(), status);
        assert!(!response.headers().contains_key("retry-after"));
        assert!(response.headers().contains_key("ratelimit"));
    }
}
