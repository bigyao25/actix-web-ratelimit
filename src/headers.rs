use crate::{config::RateLimitConfig, store::RateLimitMetadata};
use actix_web::http::header::{HeaderMap, HeaderValue, RETRY_AFTER};

// RFC 9651 Integer values have at most 15 decimal digits.
const MAX_SF_INTEGER: u128 = 999_999_999_999_999;

/// Static policy encoding is prepared once per middleware instance, only when enabled.
pub(crate) struct ResponseHeaders {
    policy: Option<(String, HeaderValue)>,
}

impl ResponseHeaders {
    pub(crate) fn new(config: &RateLimitConfig) -> Self {
        let policy = (|| {
            if config.max_requests as u128 > MAX_SF_INTEGER
                || !config.namespace.bytes().all(|b| (0x20..=0x7e).contains(&b))
            {
                return None;
            }
            let name = format!(
                "\"{}\"",
                config.namespace.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let mut policy = format!("{name};q={}", config.max_requests);
            // Structured Fields cannot express fractional-second windows. Omit w rather
            // than advertise a different policy; admission still uses the exact duration.
            if config.window_secs.subsec_nanos() == 0 {
                policy.push_str(&format!(";w={}", config.window_secs.as_secs()));
            }
            Some((name, HeaderValue::from_str(&policy).ok()?))
        })();
        Self { policy }
    }

    pub(crate) fn apply(
        &self,
        headers: &mut HeaderMap,
        metadata: RateLimitMetadata,
        rejected_with_429: bool,
    ) {
        if let Some((name, policy)) = &self.policy
            && metadata.remaining as u128 <= MAX_SF_INTEGER
        {
            if !headers.contains_key("ratelimit-policy") {
                headers.insert(
                    actix_web::http::header::HeaderName::from_static("ratelimit-policy"),
                    policy.clone(),
                );
            }
            if !headers.contains_key("ratelimit") {
                let value = HeaderValue::from_str(&format!("{name};r={}", metadata.remaining))
                    .expect("validated Structured Fields string and integer");
                headers.insert(
                    actix_web::http::header::HeaderName::from_static("ratelimit"),
                    value,
                );
            }
        }
        if rejected_with_429
            && !headers.contains_key(RETRY_AFTER)
            && let Some(delay) = metadata.retry_after
        {
            let seconds = delay
                .as_secs()
                .saturating_add(u64::from(delay.subsec_nanos() > 0));
            headers.insert(
                RETRY_AFTER,
                HeaderValue::from_str(&seconds.to_string()).expect("decimal seconds"),
            );
        }
    }
}
