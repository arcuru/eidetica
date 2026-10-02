//! Liveness registry for engine-served dial attempts.
//!
//! The integration suite's dial diagnostics assert prompt teardown of losing
//! attempts through these handles instead of allocator baselines: a handle
//! whose [`std::sync::Weak::upgrade`] returns `None` means that dial's task —
//! the connection, buffers, and registration work it holds — is gone. Under
//! winner-cancels-losers semantics (RFC 8305 section 5) every handle from a
//! race must fail `upgrade()` shortly after the winner is selected.
//!
//! The registry only grows between calls to [`reset_dial_attempts`]: a test
//! counts every attempt its race started, including ones recorded after the
//! winner, so nothing is pruned behind its back.
//!
//! Gated like the other internal testing hooks: never compiled into a
//! release build.

use std::sync::{Arc, Mutex, Weak};

pub use super::background::conn::DialAttempt;

static DIAL_ATTEMPTS: Mutex<Vec<Weak<DialAttempt>>> = Mutex::new(Vec::new());

/// Record one dial attempt's liveness handle.
pub(crate) fn record(attempt: &Arc<DialAttempt>) {
    DIAL_ATTEMPTS
        .lock()
        .expect("dial attempt registry poisoned")
        .push(Arc::downgrade(attempt));
}

/// Every recorded dial attempt, oldest first, as liveness handles.
pub fn dial_attempts() -> Vec<Weak<DialAttempt>> {
    DIAL_ATTEMPTS
        .lock()
        .expect("dial attempt registry poisoned")
        .clone()
}

/// Forget every recorded dial attempt, so a test observes only what runs
/// after this call.
pub fn reset_dial_attempts() {
    DIAL_ATTEMPTS
        .lock()
        .expect("dial attempt registry poisoned")
        .clear();
}
