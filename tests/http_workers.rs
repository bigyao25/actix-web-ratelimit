use actix_web::{App, HttpResponse, HttpServer, web};
use actix_web_ratelimit::{
    RateLimit,
    config::RateLimitConfig,
    store::{MemoryStore, RateLimitStore},
};
use std::{
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn exercise_workers(store: Arc<dyn RateLimitStore>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let workers = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let (handler_calls, handler_workers) = (calls.clone(), workers.clone());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let config = RateLimitConfig::default().max_requests(17);
    let server = HttpServer::new(move || {
        let (calls, workers) = (handler_calls.clone(), handler_workers.clone());
        App::new()
            .wrap(RateLimit::new(config.clone(), store.clone()))
            .default_service(web::to(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                workers.lock().unwrap().insert(std::thread::current().id());
                async { HttpResponse::Ok().body("accepted") }
            }))
    })
    .workers(2)
    .disable_signals()
    .listen(listener)
    .unwrap()
    .run();
    let handle = server.handle();
    let task = actix_web::rt::spawn(server);
    let responses = futures_util::future::join_all((0..128).map(|i| async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let mut stream = tokio::net::TcpStream::connect(address).await?;
            stream.write_all(format!("GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nForwarded: for=192.0.2.{i}\r\nX-Forwarded-For: 198.51.100.{i}\r\n\r\n").as_bytes()).await?;
            let mut response = String::new();
            stream.read_to_string(&mut response).await?;
            Ok::<_, std::io::Error>(response)
        }).await
    })).await;
    // Stop workers before asserting, including when a request returned an error.
    handle.stop(false).await;
    task.await.unwrap().unwrap();
    let responses: Vec<_> = responses.into_iter().map(|r| r.unwrap().unwrap()).collect();
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.starts_with("HTTP/1.1 200 "))
            .count(),
        17
    );
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.starts_with("HTTP/1.1 429 "))
            .count(),
        111
    );
    assert_eq!(calls.load(Ordering::SeqCst), 17);
    assert_eq!(workers.lock().unwrap().len(), 2);
}

#[actix_web::test]
async fn memory_quotas_are_shared_across_real_http_workers() {
    exercise_workers(Arc::new(MemoryStore::new())).await;
}

#[cfg(feature = "redis")]
#[actix_web::test]
#[ignore = "requires Redis"]
async fn redis_connection_and_quotas_are_shared_across_real_http_workers() {
    use actix_web_ratelimit::store::{FailurePolicy, RedisStore, RedisStoreOptions};
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let prefix = format!(
        "ratelimit:http-test:{}:{}:",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let store = RedisStore::with_options(
        &url,
        RedisStoreOptions {
            failure_policy: FailurePolicy::FailClosed,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_prefix(&prefix);
    exercise_workers(Arc::new(store)).await;
    let mut connection = redis::Client::open(url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    // The default ID is the peer IP, independent of all 128 source ports and forged headers.
    let key = format!("{prefix}v2:7:default:100000000000:17:9:127.0.0.1");
    assert_eq!(
        redis::cmd("DEL")
            .arg(key)
            .query_async::<usize>(&mut connection)
            .await
            .unwrap(),
        1
    );
}
