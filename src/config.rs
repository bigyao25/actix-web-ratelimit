use actix_web::{HttpResponse, dev::ServiceRequest};
use std::{fmt, time::Duration};

const MAX_EXACT_INTEGER: u128 = (1_u128 << 53) - 1;

/// Invalid sliding-window settings. A zero request limit is valid and denies all requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    ZeroWindow,
    WindowTooLarge,
    LimitTooLarge,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroWindow => "the rate limit window must be greater than zero",
            Self::WindowTooLarge => {
                "the rate limit window exceeds the exact Redis microsecond range"
            }
            Self::LimitTooLarge => "the request limit exceeds the exact Redis integer range",
        })
    }
}

impl std::error::Error for ConfigError {}

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
///             .body(format!("Rate limit exceeded for client: {}", id))
///     });
/// ```
#[derive(Clone)]
pub struct RateLimitConfig {
    /// Logical rule name. Equal namespace, window and quota share a bucket per client.
    pub namespace: String,
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
    /// - `get_id`: Extracts the peer IP, ignoring untrusted forwarding headers
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
            namespace: "default".to_owned(),
            max_requests: 10,
            window_secs: Duration::from_secs(100),
            get_id: |req| {
                req.peer_addr()
                    .map(|addr| addr.ip().to_canonical().to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            },
            on_exceed: |_id, _config, _req| {
                HttpResponse::TooManyRequests().body("Too many requests")
            },
        }
    }
}

impl RateLimitConfig {
    /// Validates the common numeric range used by both built-in backends.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.window_secs.is_zero() {
            return Err(ConfigError::ZeroWindow);
        }
        if self.window_secs.as_nanos().div_ceil(1_000) > MAX_EXACT_INTEGER {
            return Err(ConfigError::WindowTooLarge);
        }
        if self.max_requests as u128 > MAX_EXACT_INTEGER {
            return Err(ConfigError::LimitTooLarge);
        }
        Ok(())
    }

    /// Names a logical quota, such as `login` or `search`. Different names never share counters.
    pub fn namespace(mut self, value: impl Into<String>) -> Self {
        self.namespace = value.into();
        self
    }

    /// Sets the maximum number of requests allowed within the time window.
    ///
    /// # Arguments
    ///
    /// * `value` - Maximum admitted requests; zero denies all requests
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
    /// // Rate limit by custom header
    /// let config = RateLimitConfig::default()
    ///     .id(|req| {
    ///         // Extract user ID from custom header
    ///         req.headers()
    ///             .get("X-User-ID")
    ///             .and_then(|h| h.to_str().ok())
    ///             .unwrap_or("guest")
    ///             .to_string()
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
    /// use actix_web::HttpResponse;
    /// use actix_web_ratelimit::config::RateLimitConfig;
    ///
    /// // Custom error response with details
    /// let config = RateLimitConfig::default()
    ///     .exceeded(|id, config, _req| {
    ///         HttpResponse::TooManyRequests()
    ///             .body(format!(
    ///                 "Rate limit exceeded for client: {}. Limit: {} requests per {} seconds.",
    ///                 id,
    ///                 config.max_requests,
    ///                 config.window_secs.as_secs()
    ///             ))
    ///     });
    ///
    /// // Custom limit headers. Use RateLimit::response_headers(true) for an accurate Retry-After.
    /// let config = RateLimitConfig::default()
    ///     .exceeded(|_id, config, _req| {
    ///         HttpResponse::TooManyRequests()
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
