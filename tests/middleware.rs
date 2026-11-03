use actix_web::{
    App, HttpResponse,
    body::to_bytes,
    dev::{Service, ServiceRequest, Transform},
    http::StatusCode,
    test, web,
};
use actix_web_ratelimit::{
    RateLimit,
    config::RateLimitConfig,
    store::{MemoryStore, RateLimitStore, StoreError},
};
use futures_util::future::BoxFuture;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[actix_web::test]
async fn admitted_requests_reach_handler_and_rejections_use_custom_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let config = RateLimitConfig::default()
        .max_requests(1)
        .id(|req| {
            req.headers()
                .get("X-User")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .exceeded(|id, _, _| {
            HttpResponse::TooManyRequests()
                .insert_header(("X-Limited", "yes"))
                .body(id.clone())
        });
    let app = test::init_service(
        App::new()
            .wrap(RateLimit::new(config, MemoryStore::new()))
            .default_service(web::to(move || {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                async {
                    HttpResponse::Created()
                        .insert_header(("X-Handler", "yes"))
                        .body("accepted")
                }
            })),
    )
    .await;
    for (user, status) in [
        ("a", StatusCode::CREATED),
        ("a", StatusCode::TOO_MANY_REQUESTS),
        ("b", StatusCode::CREATED),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .insert_header(("X-User", user))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), status);
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(response.headers().get("X-Limited").unwrap(), "yes");
            assert_eq!(test::read_body(response).await, "a");
        } else {
            assert_eq!(response.headers().get("X-Handler").unwrap(), "yes");
            assert_eq!(test::read_body(response).await, "accepted");
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

struct DelayedStore {
    fail: bool,
    completed: Arc<AtomicUsize>,
}
impl RateLimitStore for DelayedStore {
    fn is_limited<'a>(
        &'a self,
        _: &'a str,
        _: &'a RateLimitConfig,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            actix_web::rt::time::sleep(Duration::from_millis(5)).await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(StoreError::new(std::io::Error::other(
                    "private backend detail",
                )))
            } else {
                Ok(false)
            }
        })
    }
}

#[actix_web::test]
async fn awaits_store_and_backend_errors_return_503_without_calling_handler() {
    for fail in [false, true] {
        let completed = Arc::new(AtomicUsize::new(0));
        let handler_completed = completed.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = calls.clone();
        let app = test::init_service(
            App::new()
                .wrap(RateLimit::new(
                    RateLimitConfig::default(),
                    DelayedStore {
                        fail,
                        completed: completed.clone(),
                    },
                ))
                .default_service(web::to(move || {
                    assert_eq!(handler_completed.load(Ordering::SeqCst), 1);
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    async { HttpResponse::Ok().finish() }
                })),
        )
        .await;
        let response = test::call_service(&app, test::TestRequest::get().to_request()).await;
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(
            response.status(),
            if fail {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(!fail));
        if fail {
            assert_eq!(
                test::read_body(response).await,
                "Rate limit storage unavailable"
            );
        }
    }
}

#[actix_web::test]
async fn middleware_preserves_downstream_service_errors() {
    let middleware = RateLimit::new(RateLimitConfig::default(), MemoryStore::new())
        .new_transform(actix_service::fn_service(|_: ServiceRequest| async {
            Err::<actix_web::dev::ServiceResponse, _>(actix_web::error::ErrorBadRequest(
                "downstream",
            ))
        }))
        .await
        .unwrap();
    std::future::poll_fn(|cx| middleware.poll_ready(cx))
        .await
        .unwrap();
    let error = middleware
        .call(test::TestRequest::default().to_srv_request())
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.as_response_error().status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        to_bytes(error.error_response().into_body()).await.unwrap(),
        "downstream"
    );
}

#[actix_web::test]
async fn middleware_is_shared_across_apps_and_distinct_rules_can_be_scoped() {
    let store = Arc::new(MemoryStore::new());
    let config = RateLimitConfig::default().max_requests(1);
    let first = test::init_service(
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    let second = test::init_service(
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    assert_eq!(
        test::call_service(&first, test::TestRequest::get().to_request())
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        test::call_service(&second, test::TestRequest::get().to_request())
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let third = test::init_service(
        App::new()
            .wrap(RateLimit::new(config.namespace("other"), store))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    assert_eq!(
        test::call_service(&third, test::TestRequest::get().to_request())
            .await
            .status(),
        StatusCode::OK
    );
}
