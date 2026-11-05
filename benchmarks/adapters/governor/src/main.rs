use actix_governor::{Governor, GovernorConfigBuilder, KeyExtractor, SimpleKeyExtractionError};
use actix_web::{App, HttpResponse, HttpServer, dev::ServiceRequest, web};
use std::time::Duration;
#[derive(Clone)]
struct Client;
impl KeyExtractor for Client {
    type Key = String;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;
    fn extract(&self, req: &ServiceRequest) -> Result<String, Self::KeyExtractionError> {
        Ok(req
            .headers()
            .get("x-bench-client")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0")
            .to_owned())
    }
}
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let limit: u32 = std::env::var("LIMIT").unwrap().parse().unwrap();
    assert!(limit > 0, "GCRA has no equivalent zero quota");
    let window: f64 = std::env::var("WINDOW").unwrap().parse().unwrap();
    let config = GovernorConfigBuilder::default()
        .period(Duration::from_secs_f64(window / limit as f64))
        .burst_size(limit)
        .key_extractor(Client)
        .finish()
        .unwrap();
    HttpServer::new(move || {
        App::new()
            .wrap(Governor::new(&config))
            .default_service(web::to(|| async { HttpResponse::Ok().body("ok") }))
    })
    .workers(
        std::env::var("WORKERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
    )
    .keep_alive(std::time::Duration::from_secs(120)).bind(("0.0.0.0", 8080))?
    .run()
    .await
}
