use actix_web::{App, HttpServer, HttpResponse, web};
#[cfg(not(feature = "baseline"))]
use limiter::{RateLimit, config::RateLimitConfig, store::{MemoryStore, RedisStore, RateLimitStore}};
#[cfg(not(feature = "baseline"))]
use std::sync::Arc;
fn number(name: &str, default: usize) -> usize { std::env::var(name).ok().and_then(|s|s.parse().ok()).unwrap_or(default) }
async fn handler() -> HttpResponse { HttpResponse::Ok().body("ok") }
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    #[cfg(not(feature = "baseline"))]
    let (config, store) = {
        let config = RateLimitConfig::default().max_requests(number("LIMIT", 100)).window_secs(number("WINDOW", 60) as u64)
            .id(|req|req.headers().get("x-bench-client").and_then(|v|v.to_str().ok()).unwrap_or("0").to_owned())
            .exceeded(|_,_,_|HttpResponse::TooManyRequests().body("limited"));
        let store: Arc<dyn RateLimitStore> = if std::env::var("BACKEND").unwrap_or_default() == "redis" {
            #[cfg(feature = "legacy")]
            let redis = RedisStore::new("redis://redis:6379/").expect("Redis initialization failed");
            #[cfg(not(feature = "legacy"))]
            let redis = RedisStore::new("redis://redis:6379/").await.expect("Redis initialization failed");
            Arc::new(redis.with_prefix("benchmark:"))
        } else { Arc::new(MemoryStore::new()) };
        (config, store)
    };
    HttpServer::new(move || {
        let app = App::new();
        #[cfg(not(feature = "baseline"))]
        let app = app.wrap(RateLimit::new(config.clone(), store.clone()));
        app.default_service(web::to(handler))
    }).workers(number("WORKERS", 2)).bind(("0.0.0.0", 8080))?.run().await
}
