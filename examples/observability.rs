//! Backend-agnostic observation using shared atomic counters; no Redis required.
//!
//! Terminal 1: start the server and leave it running in the foreground.
//! ```text
//! cargo run --locked --example observability --no-default-features
//! ```
//! Terminal 2: send four requests, then read the counters.
//! ```text
//! for i in 1 2 3 4
//! do
//!     curl -i 'http://127.0.0.1:8080/'
//!     printf '\n'
//! done
//! curl 'http://127.0.0.1:8080/stats'
//! ```
//! On a fresh server, expect 200, 200, 200, 429 and
//! `allowed=3 limited=1 errors=0`. Counters accumulate until the server restarts;
//! the quota recovers as admitted requests leave the 60-second sliding window.
//! The example does not log each request. Stop terminal 1 with Ctrl+C.
use actix_web::{App, HttpResponse, HttpServer, web};
use actix_web_ratelimit::{
    RateLimit,
    config::RateLimitConfig,
    observability::{RateLimitEvent, RateLimitObserver, RateLimitOutcome},
    store::MemoryStore,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Default)]
struct Counters {
    allowed: AtomicU64,
    limited: AtomicU64,
    errors: AtomicU64,
}
impl RateLimitObserver for Counters {
    fn on_event(&self, event: &RateLimitEvent<'_>) {
        // Runs synchronously after the store check and lock release, before the
        // handler. Keep it non-blocking: no disk/network I/O or blocking sends.
        // Outcomes describe admission, not the downstream handler's HTTP status.
        let counter = match event.outcome {
            RateLimitOutcome::Allowed => &self.allowed,
            RateLimitOutcome::Limited => &self.limited,
            // Aggregate failures here: StoreError returns 503; FailOpen admits
            // despite a backend failure. MemoryStore does not use fail-open.
            RateLimitOutcome::StoreError | RateLimitOutcome::FailOpen => &self.errors,
            // The public outcome enum allows future variants.
            _ => return,
        };
        // These counters do not synchronize other data, so Relaxed is sufficient.
        counter.fetch_add(1, Ordering::Relaxed);
        // event.check_duration measures only the store check, not this callback
        // or the handler. event.metadata is optional; observation alone does not
        // request quota metadata. Async export belongs in an application-managed
        // bounded, non-blocking queue with an explicit queue-full policy.
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Create both once, outside the server factory, so all workers share the
    // same observer counters and process-local quota instead of per-worker ones.
    let counters = Arc::new(Counters::default());
    let store = Arc::new(MemoryStore::new());
    HttpServer::new(move || {
        App::new()
            // Expose the same counters to the stats handler without copying them.
            .app_data(web::Data::from(counters.clone()))
            .service(
                web::resource("/")
                    .wrap(
                        RateLimit::new(
                            RateLimitConfig::default()
                                // Use a stable rule name, never client IDs or secrets.
                                .namespace("api")
                                .max_requests(3)
                                .window_secs(60),
                            store.clone(),
                        )
                        // Observation is opt-in and independent of response headers.
                        // Add .response_headers(true) separately if headers are wanted.
                        .observe(counters.clone()),
                    )
                    .to(|| async { HttpResponse::Ok().body("accepted") }),
            )
            // Outside the wrapped resource: reading stats consumes no quota and
            // emits no event. Protect or restrict this endpoint in production.
            .route(
                "/stats",
                web::get().to(|stats: web::Data<Counters>| async move {
                    // Independent atomic reads, not a transactional snapshot;
                    // concurrent requests may update counts between these loads.
                    HttpResponse::Ok().body(format!(
                        "allowed={} limited={} errors={}\n",
                        stats.allowed.load(Ordering::Relaxed),
                        stats.limited.load(Ordering::Relaxed),
                        stats.errors.load(Ordering::Relaxed)
                    ))
                }),
            )
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
