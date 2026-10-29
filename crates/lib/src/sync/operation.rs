//! Process-local diagnostics for one outbound sync operation. Never sent on the wire.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{Span, info, info_span};

use super::{Address, TimeoutPhase};
use crate::{Error, Result};

/// Route selection and the exchange(s) it serves. Counters describe results
/// observed by the selector, not the eventual outcome of losing dial tasks.
pub(super) struct SyncOperation {
    span: Span,
    routes: usize,
    attempted: usize,
    failed: usize,
    observed: usize,
    timed_out: usize,
    wrong_peer: usize,
    selected: Option<Address>,
    trees_ok: usize,
    trees_failed: usize,
    last_tree_error: Option<String>,
    last_tree_timeout: Option<TimeoutPhase>,
}

impl SyncOperation {
    pub(super) fn new(kind: &'static str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        // No UUID generation, allocation or shared counter traffic without tracing.
        let span = if tracing::enabled!(tracing::Level::INFO) {
            let operation_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            info_span!("sync_operation", operation_id, kind)
        } else {
            Span::none()
        };
        Self {
            span,
            routes: 0,
            attempted: 0,
            failed: 0,
            observed: 0,
            timed_out: 0,
            wrong_peer: 0,
            selected: None,
            trees_ok: 0,
            trees_failed: 0,
            last_tree_error: None,
            last_tree_timeout: None,
        }
    }

    pub(super) fn span(&self) -> Span {
        self.span.clone()
    }

    pub(super) fn routes(&mut self, available: usize, attempted: usize) {
        self.routes += available;
        self.attempted += attempted;
    }

    // The outer race deadline includes queuing, handshake and registration;
    // unlike a transport timeout, it cannot establish a connection phase.
    pub(super) fn failed_route(&mut self, error: &Error, attempt_timed_out: bool) {
        self.failed += 1;
        self.observed += 1;
        self.timed_out +=
            usize::from(attempt_timed_out || matches!(error, Error::Sync(e) if e.is_timeout()));
    }

    pub(super) fn wrong_peer(&mut self) {
        self.wrong_peer += 1;
        self.observed += 1;
    }

    pub(super) fn selected(&mut self, address: &Address) {
        self.observed += 1;
        if !self.span.is_disabled() {
            self.selected = Some(address.clone());
        }
    }

    pub(super) fn tree_result(&mut self, result: &Result<()>) {
        if result.is_ok() {
            self.trees_ok += 1;
        } else {
            self.trees_failed += 1;
            if !self.span.is_disabled() {
                self.last_tree_error = result.as_ref().err().map(ToString::to_string);
                self.last_tree_timeout = result.as_ref().err().and_then(|e| match e {
                    Error::Sync(e) => e.timeout_phase(),
                    _ => None,
                });
            }
        }
    }

    pub(super) fn finish<T>(&self, result: &Result<T>) {
        let outcome = match (result.is_ok(), self.trees_ok, self.trees_failed) {
            (false, _, _) | (true, 0, 1..) => "failed",
            (true, 1.., 1..) => "partial",
            _ => "success",
        };
        let error = result.as_ref().err();
        let timeout_phase = error
            .and_then(|e| match e {
                Error::Sync(e) => e.timeout_phase(),
                _ => None,
            })
            .or(self.last_tree_timeout);
        let _entered = self.span.enter();
        info!(
            outcome,
            routes = self.routes,
            attempted = self.attempted,
            route_failures = self.failed,
            route_timeouts = self.timed_out,
            wrong_peer = self.wrong_peer,
            // Includes cancelled or queued results not consumed before selection.
            unobserved = self.attempted - self.observed,
            unsupported = self.routes - self.attempted,
            selected = ?self.selected,
            trees_ok = self.trees_ok,
            trees_failed = self.trees_failed,
            error = ?error,
            last_tree_error = ?self.last_tree_error,
            timeout_phase = ?timeout_phase,
            peer_reachable = ?timeout_phase.map(|phase: TimeoutPhase| phase.peer_reachable()),
            "Sync route outcome"
        );
    }
}
