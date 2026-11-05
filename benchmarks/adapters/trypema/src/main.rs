use actix_trypema::{Local, Redis, TrypemaLimiter, extract::Header};
use actix_web::{App, HttpResponse, HttpServer, web};
use trypema::{RateLimit, WindowSize};
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let limit: f64 = std::env::var("LIMIT").unwrap().parse().unwrap();
    let window: u64 = std::env::var("WINDOW").unwrap().parse().unwrap();
    let workers = std::env::var("WORKERS")
        .unwrap_or("2".into())
        .parse()
        .unwrap();
    macro_rules! serve {
        ($backend:expr) => {{
            let limiter = TrypemaLimiter::builder($backend)
                .namespace("benchmark")
                .extractor(Header::new("x-bench-client").unwrap())
                .rate(RateLimit::per_second_or_panic(limit / window as f64))
                .build()
                .unwrap();
            HttpServer::new(move || {
                App::new()
                    .wrap(limiter.clone())
                    .default_service(web::to(|| async { HttpResponse::Ok().body("ok") }))
            })
            .workers(workers)
            .bind(("0.0.0.0", 8080))?
            .run()
            .await
        }};
    }
    let window = WindowSize::seconds_or_panic(window);
    if std::env::var("BACKEND").unwrap() == "redis" {
        let connection = redis::Client::open("redis://redis:6379/")
            .unwrap()
            .get_connection_manager()
            .await
            .unwrap();
        serve!(Redis::new(connection, window).unwrap())
    } else {
        serve!(Local::new(window).unwrap())
    }
}
