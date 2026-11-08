//! In-process middleware costs; request construction is included, network I/O is not.
//! Reports five-sample medians for each outcome, header and observer mode.
use actix_web::{App, HttpResponse, http::StatusCode, test, web};
use actix_web_ratelimit::observability::RateLimitEvent;
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
use std::{
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

fn main() {
    if cfg!(debug_assertions) {
        return;
    }
    actix_web::rt::System::new().block_on(benchmark());
}

async fn benchmark() {
    const REQUESTS: usize = 100_000;
    const WARMUP: usize = 1_000;
    for rejected in [false, true] {
        for enabled in [false, true] {
            for observer in ["off", "noop", "atomic"] {
                let counter = Arc::new(AtomicU64::new(0));
                let mut samples = Vec::new();
                for _ in 0..5 {
                    let config = RateLimitConfig::default()
                        .max_requests(if rejected { 1 } else { REQUESTS + WARMUP })
                        .window_secs(3600);
                    let mut middleware =
                        RateLimit::new(config, MemoryStore::new()).response_headers(enabled);
                    match observer {
                        "noop" => middleware = middleware.observe(|_: &RateLimitEvent<'_>| {}),
                        "atomic" => {
                            let counter = counter.clone();
                            middleware = middleware.observe(move |_: &RateLimitEvent<'_>| {
                                counter.fetch_add(1, Ordering::Relaxed);
                            });
                        }
                        _ => {}
                    }
                    let app = test::init_service(
                        App::new()
                            .wrap(middleware)
                            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
                    )
                    .await;
                    for _ in 0..WARMUP {
                        black_box(
                            test::call_service(&app, test::TestRequest::get().to_request()).await,
                        );
                    }
                    let start = Instant::now();
                    for _ in 0..REQUESTS {
                        black_box(
                            test::call_service(&app, test::TestRequest::get().to_request()).await,
                        );
                    }
                    samples.push(start.elapsed().as_nanos() as f64 / REQUESTS as f64);
                    let response =
                        test::call_service(&app, test::TestRequest::get().to_request()).await;
                    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                }
                samples.sort_by(f64::total_cmp);
                println!(
                    "rejected={rejected} headers={enabled} observer={observer} median_ns_per_request={:.0}",
                    samples[2]
                );
                black_box(counter.load(Ordering::Relaxed));
            }
        }
    }
}
