use actix_web::HttpResponse;
use actix_web::{App, HttpServer, Responder, web};
use actix_web_ratelimit::config::RateLimitConfig;
use actix_web_ratelimit::{RateLimit, store::MemoryStore};
use std::sync::Arc;

async fn index() -> impl Responder {
    "Hello world!"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let store = Arc::new(MemoryStore::new());
    let config = RateLimitConfig::default()
        .max_requests(3)
        .window_secs(10)
        .id(|req| {
            // Custom client identification
            req.headers()
                .get("X-Client-Id")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("anonymous")
                .to_string()
        })
        .exceeded(|id, config, _req| {
            // Custom rate limit exceeded response
            HttpResponse::TooManyRequests().body(format!(
                "429 caused: client-id: {}, limit: {}req/{:?}",
                id, config.max_requests, config.window_secs
            ))
        });

    HttpServer::new(move || {
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
