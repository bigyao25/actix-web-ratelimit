//! Optional, backend-agnostic hooks for completed rate-limit checks.
//!
//! Events contain no client identifiers or raw backend errors. Keep namespaces
//! low-cardinality and free of sensitive values. No exporter or task is started.

use crate::store::{RateLimitDecision, RateLimitMetadata};
use std::{sync::Arc, time::Duration};

/// The actual storage/admission path, independent of downstream HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateLimitOutcome {
    Allowed,
    Limited,
    StoreError,
    FailOpen,
}

/// Coarse error categories, without backend messages or connection details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateLimitErrorKind {
    Timeout,
    Connection,
    Backend,
    Other,
}

/// Borrowed event emitted once after a completed check and before the handler.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RateLimitEvent<'a> {
    pub namespace: &'a str,
    pub outcome: RateLimitOutcome,
    /// Monotonic elapsed time for the check, excluding the callback and handler.
    pub check_duration: Duration,
    pub max_requests: usize,
    pub window: Duration,
    /// Only metadata already requested for response headers; never queried again.
    pub metadata: Option<RateLimitMetadata>,
    pub error_kind: Option<RateLimitErrorKind>,
}

/// Synchronous observer shared across workers.
///
/// Callbacks run after storage locks are released and cannot change admission.
/// Keep them fast and non-blocking: do not perform disk/network I/O, wait on
/// contended mutexes, or use blocking channel sends. Prefer atomics or an
/// application-owned bounded, non-blocking queue. Applications own async export,
/// queue-full, backpressure and drop policies.
///
/// Unwinding callback panics are caught (the process panic hook still runs).
/// `panic=abort`, aborting panic hooks and double panics are not recoverable.
/// A caught panic payload is deliberately leaked so a user-defined payload
/// destructor cannot panic again and change admission. Repeated panics can leak
/// memory; panicking is not a supported event delivery strategy.
pub trait RateLimitObserver: Send + Sync {
    fn on_event(&self, event: &RateLimitEvent<'_>);
}

impl<F> RateLimitObserver for F
where
    F: Fn(&RateLimitEvent<'_>) + Send + Sync,
{
    fn on_event(&self, event: &RateLimitEvent<'_>) {
        self(event);
    }
}

impl<T: RateLimitObserver + ?Sized> RateLimitObserver for Arc<T> {
    fn on_event(&self, event: &RateLimitEvent<'_>) {
        (**self).on_event(event);
    }
}

/// Store extension result. Existing decision struct literals remain compatible.
///
/// Custom stores normally return `decision.into()`. A fail-open result should
/// have `decision.allowed = true`, no unreliable quota metadata, and (when
/// available) a coarse error kind. Only set `fail_open` for a real backend failure.
#[derive(Debug)]
pub struct ObservedRateLimitDecision {
    pub decision: RateLimitDecision,
    /// True only when a backend failure explicitly triggered fail-open.
    pub fail_open: bool,
    pub error_kind: Option<RateLimitErrorKind>,
}

impl From<RateLimitDecision> for ObservedRateLimitDecision {
    fn from(decision: RateLimitDecision) -> Self {
        Self {
            decision,
            fail_open: false,
            error_kind: None,
        }
    }
}

// Only the opt-in store decorator instantiates this future. The default
// middleware and store path retain their original layout and implementation.
async fn check_observed(
    store: &dyn crate::store::RateLimitStore,
    id: &str,
    config: &crate::config::RateLimitConfig,
    metadata_requested: bool,
    observer: &dyn RateLimitObserver,
) -> Result<RateLimitDecision, crate::store::StoreError> {
    let start = std::time::Instant::now();
    let observed = store.check_observed(id, config, metadata_requested).await;
    let check_duration = start.elapsed();
    let (outcome, metadata, error_kind) = match &observed {
        Ok(value) => (
            if value.fail_open {
                RateLimitOutcome::FailOpen
            } else if value.decision.allowed {
                RateLimitOutcome::Allowed
            } else {
                RateLimitOutcome::Limited
            },
            value.decision.metadata,
            value.error_kind,
        ),
        Err(error) => (RateLimitOutcome::StoreError, None, Some(error.kind())),
    };
    let event = RateLimitEvent {
        namespace: &config.namespace,
        outcome,
        check_duration,
        max_requests: config.max_requests,
        window: config.window_secs,
        metadata,
        error_kind,
    };
    // Only callback panics are isolated, not storage/handler panics.
    // Forget a panic payload: its user-defined destructor may panic too.
    if let Err(payload) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.on_event(&event)))
    {
        std::mem::forget(payload);
    }
    observed.map(|value| value.decision)
}

/// Installed once during middleware initialization, only when observation is enabled.
pub(crate) struct ObservedStore {
    pub store: Arc<dyn crate::store::RateLimitStore>,
    pub observer: Arc<dyn RateLimitObserver>,
}

impl crate::store::RateLimitStore for ObservedStore {
    fn is_limited<'a>(
        &'a self,
        key: &'a str,
        config: &'a crate::config::RateLimitConfig,
    ) -> futures_util::future::BoxFuture<'a, Result<bool, crate::store::StoreError>> {
        Box::pin(async move {
            check_observed(
                self.store.as_ref(),
                key,
                config,
                false,
                self.observer.as_ref(),
            )
            .await
            .map(|decision| !decision.allowed)
        })
    }

    fn check<'a>(
        &'a self,
        key: &'a str,
        config: &'a crate::config::RateLimitConfig,
    ) -> futures_util::future::BoxFuture<'a, Result<RateLimitDecision, crate::store::StoreError>>
    {
        Box::pin(check_observed(
            self.store.as_ref(),
            key,
            config,
            true,
            self.observer.as_ref(),
        ))
    }
}
