use actix_web::test::TestRequest;
use actix_web_ratelimit::{
    RateLimit,
    config::{ConfigError, RateLimitConfig},
    store::MemoryStore,
};
use std::{net::SocketAddr, time::Duration};

#[test]
fn default_identity_ignores_forwarding_headers_and_source_ports() {
    let config = RateLimitConfig::default();
    for port in [1234, 5678] {
        let req = TestRequest::default()
            .peer_addr(SocketAddr::from(([192, 0, 2, 1], port)))
            .insert_header(("Forwarded", "for=203.0.113.1"))
            .insert_header(("X-Forwarded-For", "203.0.113.2"))
            .to_srv_request();
        assert_eq!((config.get_id)(&req), "192.0.2.1");
    }
    let req = TestRequest::default()
        .peer_addr("[::ffff:192.0.2.1]:80".parse().unwrap())
        .to_srv_request();
    assert_eq!((config.get_id)(&req), "192.0.2.1");
    let req = TestRequest::default()
        .peer_addr("[2001:db8::1]:80".parse().unwrap())
        .to_srv_request();
    assert_eq!((config.get_id)(&req), "2001:db8::1");
    assert_eq!(
        (config.get_id)(&TestRequest::default().to_srv_request()),
        "unknown"
    );
}

#[test]
fn configuration_boundaries_are_explicit() {
    assert!(
        RateLimitConfig::default()
            .max_requests(0)
            .validate()
            .is_ok()
    );
    assert_eq!(
        RateLimitConfig::default().window_secs(0).validate(),
        Err(ConfigError::ZeroWindow)
    );
    assert_eq!(
        RateLimitConfig::default().window_secs(u64::MAX).validate(),
        Err(ConfigError::WindowTooLarge)
    );
    let mut config = RateLimitConfig {
        window_secs: Duration::from_nanos(1),
        ..Default::default()
    };
    assert!(config.validate().is_ok());
    config.window_secs = Duration::from_micros((1 << 53) - 1);
    assert!(config.validate().is_ok());
    config.window_secs += Duration::from_nanos(1);
    assert_eq!(config.validate(), Err(ConfigError::WindowTooLarge));
    if usize::BITS >= 64 {
        assert_eq!(
            RateLimitConfig::default()
                .max_requests(usize::MAX)
                .validate(),
            Err(ConfigError::LimitTooLarge)
        );
    }
}

#[test]
fn fallible_middleware_constructor_rejects_invalid_configuration() {
    assert_eq!(
        RateLimit::try_new(
            RateLimitConfig::default().window_secs(0),
            MemoryStore::new()
        )
        .err(),
        Some(ConfigError::ZeroWindow)
    );
}
