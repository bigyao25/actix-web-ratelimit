use actix_web::{App, HttpServer, Responder, web};
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};
use std::sync::Arc;

async fn index() -> impl Responder {
    "Hello world!"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);
    let store = Arc::new(MemoryStore::new());

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
