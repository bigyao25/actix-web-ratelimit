use std::sync::Arc;

use actix_web::{App, HttpServer, Responder, web};
use actix_web_ratelimit::{RateLimit, config::RateLimitConfig, store::MemoryStore};

async fn index() -> impl Responder {
    "Hello world!"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Configure rate limiting: allow 3 requests per 10-second window
    let config = RateLimitConfig::default().max_requests(3).window_secs(10);

    // Create in-memory store for tracking request timestamps
    // Using Arc for shared ownership across worker threads
    let store = Arc::new(MemoryStore::new());

    println!("🚀 Starting server at http://127.0.0.1:8080");
    println!(
        "📊 Rate limit: {} requests per {} seconds",
        config.max_requests,
        config.window_secs.as_secs()
    );
    println!("🧪 Test with: curl http://localhost:8080/");

    HttpServer::new(move || {
        App::new()
            // Apply rate limiting middleware to all routes
            // Note: We clone config and store for each worker thread
            // This is necessary because HttpServer creates multiple worker threads,
            // and each needs its own reference to the shared(same) config and store
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .route("/", web::get().to(index))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
