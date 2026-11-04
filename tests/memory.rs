use actix_web_ratelimit::{
    config::RateLimitConfig,
    store::{MemoryStore, RateLimitStore},
};
use futures_util::FutureExt;
use std::sync::{Arc, Barrier};

#[test]
fn admits_exactly_the_configured_limit() {
    let store = MemoryStore::new();
    let config = RateLimitConfig::default().max_requests(3);
    let allowed = (0..20)
        .filter(|_| {
            !store
                .is_limited("client", &config)
                .now_or_never()
                .unwrap()
                .unwrap()
        })
        .count();
    assert_eq!(allowed, 3);
}

#[test]
fn concurrent_requests_cannot_exceed_the_limit() {
    let store = Arc::new(MemoryStore::new());
    let barrier = Arc::new(Barrier::new(32));
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                !store
                    .is_limited("shared", &RateLimitConfig::default().max_requests(7))
                    .now_or_never()
                    .unwrap()
                    .unwrap()
            })
        })
        .collect();
    let allowed = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(allowed, 7);
}

#[actix_web::test]
async fn clients_namespaces_and_settings_have_independent_buckets() {
    let store = MemoryStore::new();
    let config = RateLimitConfig::default().max_requests(1);
    assert!(!store.is_limited("a", &config).await.unwrap());
    assert!(store.is_limited("a", &config).await.unwrap());
    assert!(!store.is_limited("b", &config).await.unwrap());
    assert!(
        !store
            .is_limited("a", &config.clone().namespace("other"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .is_limited("a", &config.clone().window_secs(2))
            .await
            .unwrap()
    );
    assert!(
        !store
            .is_limited("a", &config.clone().max_requests(2))
            .await
            .unwrap()
    );
}

#[actix_web::test]
async fn zero_limit_rejects_and_invalid_window_returns_error() {
    let store = MemoryStore::new();
    assert!(
        store
            .is_limited("a", &RateLimitConfig::default().max_requests(0))
            .await
            .unwrap()
    );
    assert!(
        store
            .is_limited("a", &RateLimitConfig::default().window_secs(0))
            .await
            .is_err()
    );
}

#[actix_web::test]
async fn shared_and_boxed_trait_objects_delegate_to_the_same_state() {
    let store: Arc<dyn RateLimitStore> = Arc::new(MemoryStore::new());
    let boxed: Box<dyn RateLimitStore> = Box::new(store.clone());
    let config = RateLimitConfig::default().max_requests(1);
    assert!(!boxed.is_limited("a", &config).await.unwrap());
    assert!(store.is_limited("a", &config).await.unwrap());
}

#[test]
fn concurrent_sweeps_preserve_active_quotas() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let store = Arc::new(MemoryStore::new());
    let barrier = Arc::new(Barrier::new(9));
    let admitted: Arc<[AtomicUsize; 8]> = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let (store, barrier, admitted) = (store.clone(), barrier.clone(), admitted.clone());
        handles.push(std::thread::spawn(move || {
            let config = RateLimitConfig::default().max_requests(50);
            barrier.wait();
            for i in 0..1_000 {
                let key = i % 8;
                if !store
                    .is_limited(&key.to_string(), &config)
                    .now_or_never()
                    .unwrap()
                    .unwrap()
                {
                    admitted[key].fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    barrier.wait();
    for _ in 0..1_000 {
        assert_eq!(store.prune_expired(), 0);
        std::thread::yield_now();
    }
    for handle in handles {
        handle.join().unwrap();
    }
    for count in admitted.iter() {
        assert_eq!(count.load(Ordering::SeqCst), 50);
    }
}
