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

#[test]
fn concurrent_detailed_checks_return_atomic_post_admission_snapshots() {
    let store = Arc::new(MemoryStore::new());
    let barrier = Arc::new(Barrier::new(32));
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .check("shared", &RateLimitConfig::default().max_requests(7))
                    .now_or_never()
                    .unwrap()
                    .unwrap()
            })
        })
        .collect();
    let mut remaining = Vec::new();
    for handle in handles {
        let decision = handle.join().unwrap();
        let metadata = decision.metadata.unwrap();
        if decision.allowed {
            remaining.push(metadata.remaining);
            assert_eq!(metadata.retry_after, None);
        } else {
            assert_eq!(metadata.remaining, 0);
            assert!(metadata.retry_after.unwrap() <= std::time::Duration::from_secs(100));
        }
    }
    remaining.sort_unstable();
    assert_eq!(remaining, (0..7).collect::<Vec<_>>());
}

#[actix_web::test]
async fn detailed_and_basic_checks_share_state_and_validate_the_same_config() {
    let store = MemoryStore::new();
    let config = RateLimitConfig::default().max_requests(2);
    assert!(!store.is_limited("a", &config).await.unwrap());
    let decision = store.check("a", &config).await.unwrap();
    assert!(decision.allowed);
    assert_eq!(decision.metadata.unwrap().remaining, 0);
    assert!(store.is_limited("a", &config).await.unwrap());
    assert!(!store.check("a", &config).await.unwrap().allowed);
    assert!(
        store
            .check("a", &config.clone().window_secs(0))
            .await
            .is_err()
    );
}
