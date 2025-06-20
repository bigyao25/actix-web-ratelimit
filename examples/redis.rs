#[cfg(feature = "redis")]
use actix_web::{App, HttpServer, Responder, web};
#[cfg(feature = "redis")]
use actix_web_ratelimit::store::RedisStore;
#[cfg(feature = "redis")]
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig};
#[cfg(feature = "redis")]
use std::sync::Arc;

#[cfg(feature = "redis")]
async fn index() -> impl Responder {
    "Hello world!"
}

#[cfg(feature = "redis")]
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let store = Arc::new(
        RedisStore::new("redis://127.0.0.1/0")
            .expect("Failed to connect to Redis")
            .with_prefix("myapp:ratelimit:"),
    );
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}

#[cfg(not(feature = "redis"))]
fn main() {}
