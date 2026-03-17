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
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    let store = Arc::new(
        // redis://[<username>][:<password>@]<hostname>[:port][/<db>]
        RedisStore::new("redis://127.0.0.1/0")
            .expect("Failed to connect to Redis")
            // Custom prefix for Redis keys
            .with_prefix("myapp:ratelimit:"),
    );

    println!("🚀 Starting REDIS server at http://127.0.0.1:8080");
    println!(
        "📊 Rate limit: {} requests per {} seconds",
        config.max_requests,
        config.window_secs.as_secs()
    );
    println!("🧪 Test with: curl http://localhost:8080/");

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
