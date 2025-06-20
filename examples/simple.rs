use std::sync::Arc;

use actix_web::{App, HttpServer, Responder, web};
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};

async fn index() -> impl Responder {
    "Hello world!"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Configure the core indicator parameters of the middleware.
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    // Where are real-time request records stored.
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            // Create and register the rate limit middleware.
            // If you don't want to clone the config and store, you can move the defination of
            // config and store into this closure.
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
