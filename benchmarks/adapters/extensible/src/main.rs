use actix_extensible_rate_limit::{
    RateLimiter,
    backend::{SimpleInputFunctionBuilder, memory::InMemoryBackend, redis::RedisBackend},
};
use actix_web::{App, HttpResponse, HttpServer, web};
use std::time::Duration;
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let limit: u64 = std::env::var("LIMIT").unwrap().parse().unwrap();
    let window: u64 = std::env::var("WINDOW").unwrap().parse().unwrap();
    let workers = std::env::var("WORKERS")
        .unwrap_or("2".into())
        .parse()
        .unwrap();
    macro_rules! serve {
        ($backend:expr) => {{
            let backend = $backend;
            HttpServer::new(move || {
                let input = SimpleInputFunctionBuilder::new(Duration::from_secs(window), limit)
                    .custom_fn(|req| {
                        Ok(req
                            .headers()
                            .get("x-bench-client")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("0")
                            .to_owned())
                    })
                    .build();
                App::new()
                    .wrap(RateLimiter::builder(backend.clone(), input).build())
                    .default_service(web::to(|| async { HttpResponse::Ok().body("ok") }))
            })
            .workers(workers)
            .bind(("0.0.0.0", 8080))?
            .run()
            .await
        }};
    }
    if std::env::var("BACKEND").unwrap() == "redis" {
        let connection = redis::Client::open("redis://redis:6379/")
            .unwrap()
            .get_connection_manager()
            .await
            .unwrap();
        serve!(RedisBackend::builder(connection).build())
    } else {
        serve!(InMemoryBackend::builder().build())
    }
}
