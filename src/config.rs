use actix_web::{HttpResponse, dev::ServiceRequest};
use std::time::Duration;

/// Configuration for rate limiting middleware.
///
/// This struct contains all the parameters needed to configure rate limiting behavior,
/// including request limits, time windows, and callback functions for client identification
/// and rate limit exceeded handling.
///
/// # Examples
///
/// ```rust
/// use actix_web::HttpResponse;
/// use actix_web_ratelimit::config::RateLimitConfig;
///
/// // Basic configuration
/// let config = RateLimitConfig::default()
///     .max_requests(100)
///     .window_secs(3600);
///
/// // Advanced configuration with custom handlers
/// let config = RateLimitConfig::default()
///     .max_requests(10)
///     .window_secs(60)
///     .id(|req| {
///         // Custom client identification based on API key
///         req.headers()
///             .get("X-API-Key")
///             .and_then(|h| h.to_str().ok())
///             .unwrap_or("anonymous")
///             .to_string()
///     })
///     .exceeded(|id, _config, _req| {
///         // Custom rate limit exceeded response
///         HttpResponse::TooManyRequests()
///             .json(serde_json::json!({
///                 "error": "Rate limit exceeded",
///                 "client_id": id
///             }))
///     });
/// ```
#[derive(Clone)]
pub struct RateLimitConfig {
    /// Maximum number of requests allowed within the time window
    pub max_requests: usize,
    /// Duration of the sliding time window
    pub window_secs: Duration,
    /// Function to extract client identifier from the request.
    /// Typically extracts IP address, but can be customized for API keys, user IDs, etc.
    pub get_id: fn(req: &ServiceRequest) -> String,
    /// Function called when rate limit is exceeded.
    /// Receives the client ID, configuration, and request, returns the HTTP response.
    pub on_exceed: fn(id: &String, config: &RateLimitConfig, req: &ServiceRequest) -> HttpResponse,
}

impl Default for RateLimitConfig {
    /// Creates a default rate limiting configuration.
    ///
    /// # Default Values
    ///
    /// - `max_requests`: 10 requests
    /// - `window_secs`: 100 seconds
    /// - `get_id`: Extracts real IP address from connection info
    /// - `on_exceed`: Returns HTTP 429 "Too Many Requests" with plain text body
    ///
    /// # Example
    ///
    /// ```rust
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// let config = RateLimitConfig::default();
    /// assert_eq!(config.max_requests, 10);
    /// ```
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
                    .body("Too many requests")
            },
        }
    }
}

impl RateLimitConfig {
    /// Sets the maximum number of requests allowed within the time window.
    ///
    /// # Arguments
    ///
    /// * `value` - Maximum number of requests (must be > 0)
    ///
    /// # Example
    ///
    /// ```rust
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// let config = RateLimitConfig::default().max_requests(100);
    /// ```
    pub fn max_requests(mut self, value: usize) -> Self {
        self.max_requests = value;
        Self { ..self }
    }

    /// Sets the time window duration in seconds for the sliding window algorithm.
    ///
    /// # Arguments
    ///
    /// * `value` - Time window duration in seconds
    ///
    /// # Example
    ///
    /// ```rust
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// // Allow 100 requests per hour
    /// let config = RateLimitConfig::default()
    ///     .max_requests(100)
    ///     .window_secs(3600);
    /// ```
    pub fn window_secs(mut self, value: u64) -> Self {
        self.window_secs = Duration::from_secs(value);
        Self { ..self }
    }

    /// Sets a custom function to extract client identifier from requests.
    ///
    /// By default, the middleware uses the client's IP address as identifier.
    /// This method allows customization based on headers, authentication, etc.
    ///
    /// # Arguments
    ///
    /// * `fn_id` - Function that takes a `ServiceRequest` and returns a client identifier string
    ///
    /// # Examples
    ///
    /// ```rust
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// // Rate limit by API key
    /// let config = RateLimitConfig::default()
    ///     .id(|req| {
    ///         req.headers()
    ///             .get("X-API-Key")
    ///             .and_then(|h| h.to_str().ok())
    ///             .unwrap_or("anonymous")
    ///             .to_string()
    ///     });
    ///
    /// // Rate limit by user ID from authentication
    /// let config = RateLimitConfig::default()
    ///     .id(|req| {
    ///         // Extract user ID from authentication middleware
    ///         req.extensions()
    ///             .get::<String>()
    ///             .cloned()
    ///             .unwrap_or_else(|| "guest".to_string())
    ///     });
    /// ```
    pub fn id(mut self, fn_id: fn(req: &ServiceRequest) -> String) -> Self {
        self.get_id = fn_id;
        Self { ..self }
    }

    /// Sets a custom function to handle rate limit exceeded scenarios.
    ///
    /// By default, returns HTTP 429 with "Too many requests" message.
    /// This method allows customization of the response format, headers, etc.
    ///
    /// # Arguments
    ///
    /// * `fn_exceed` - Function that takes client ID, config, and request, returns HTTP response
    ///
    /// # Examples
    ///
    /// ```rust
    /// use actix_web::{HttpResponse, web::Json};
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// // JSON error response
    /// let config = RateLimitConfig::default()
    ///     .exceeded(|id, config, _req| {
    ///         HttpResponse::TooManyRequests()
    ///             .json(serde_json::json!({
    ///                 "error": "Rate limit exceeded",
    ///                 "client_id": id,
    ///                 "limit": config.max_requests,
    ///                 "window_secs": config.window_secs.as_secs()
    ///             }))
    ///     });
    ///
    /// // Custom headers and retry-after
    /// let config = RateLimitConfig::default()
    ///     .exceeded(|_id, config, _req| {
    ///         HttpResponse::TooManyRequests()
    ///             .append_header(("Retry-After", config.window_secs.as_secs()))
    ///             .append_header(("X-RateLimit-Limit", config.max_requests))
    ///             .body("Rate limit exceeded. Please try again later.")
    ///     });
    /// ```
    pub fn exceeded(
        mut self,
        fn_exceed: fn(id: &String, config: &RateLimitConfig, req: &ServiceRequest) -> HttpResponse,
    ) -> Self {
        self.on_exceed = fn_exceed;
        Self { ..self }
    }
}
