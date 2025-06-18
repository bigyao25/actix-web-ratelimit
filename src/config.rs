use actix_web::{HttpResponse, dev::ServiceRequest};
use std::time::Duration;

#[derive(Clone)]
pub struct RateLimitConfig {
    pub max_requests: usize,
    pub window_secs: Duration,
    pub get_id: fn(req: &ServiceRequest) -> String,
    /// fn fired on exceeded.
    pub on_exceed: fn(id: &String, config: &RateLimitConfig, req: &ServiceRequest) -> HttpResponse,
}

impl Default for RateLimitConfig {
    /// default implement: max_requests=10, window_secs=100
    fn default() -> Self {
        Self {
            max_requests: 10,
            window_secs: Duration::from_secs(100),
            get_id: |req| {
                req.connection_info()
                    .realip_remote_addr()
                    .unwrap_or("-")
                    .to_string()
            },
            on_exceed: |_id, _config, _req| {
                HttpResponse::TooManyRequests()
                    // .append_header(("Retry-After", sec))
                    .body("Too many requests")
            },
        }
    }
}

impl RateLimitConfig {
    pub fn max_requests(mut self, value: usize) -> Self {
        self.max_requests = value;
        Self { ..self }
    }

    pub fn window_secs(mut self, value: u64) -> Self {
        self.window_secs = Duration::from_secs(value);
        Self { ..self }
    }

    /// define a fn to get an identifier, typically is IP.
    pub fn id(mut self, fn_id: fn(req: &ServiceRequest) -> String) -> Self {
        self.get_id = fn_id;
        Self { ..self }
    }

    /// define a fn fired on exceeded.
    pub fn exceeded(
        mut self,
        fn_exceed: fn(id: &String, config: &RateLimitConfig, req: &ServiceRequest) -> HttpResponse,
    ) -> Self {
        self.on_exceed = fn_exceed;
        Self { ..self }
    }
}
