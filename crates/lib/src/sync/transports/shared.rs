//! Shared utilities for transport implementations.
//!
//! This module provides common functionality used across different transport
//! implementations to reduce code duplication and ensure consistency.

use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::sync::error::SyncError;

/// Manages server state common to all transport implementations.
///
/// Transports are shared rather than owned exclusively, so that an outbound
/// request can be served from a clone while the engine goes on handling other
/// commands. That rules out `&mut self`, so the mutable parts live behind a
/// lock. It is a `std` mutex deliberately: every critical section here is a
/// few field assignments with no await inside, so an async mutex would buy
/// nothing and cost a scheduling point.
pub struct ServerState {
    inner: Mutex<ServerStateInner>,
}

/// A cancellation-safe reservation for starting one server.
pub(super) struct ServerStart<'a> {
    state: &'a ServerState,
    completed: bool,
}

struct ServerStateInner {
    /// Whether the server is starting or running.
    active: bool,
    /// Whether startup completed and the server is accepting requests.
    running: bool,
    /// Shutdown signal for the server loop.
    shutdown: Option<oneshot::Sender<()>>,
    /// The server's address.
    address: Option<String>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerState {
    /// Create a new server state manager.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(ServerStateInner {
                active: false,
                running: false,
                shutdown: None,
                address: None,
            }),
        }
    }

    /// Reserve this transport for startup.
    ///
    /// Dropping the returned guard before startup completes releases the
    /// reservation, including when the startup future is cancelled.
    pub(super) fn begin_start(&self, address: &str) -> Result<ServerStart<'_>, SyncError> {
        let mut inner = self.lock();
        if inner.active {
            return Err(SyncError::ServerAlreadyRunning {
                address: address.to_string(),
            });
        }
        inner.active = true;
        Ok(ServerStart {
            state: self,
            completed: false,
        })
    }

    /// Check if the server is currently running.
    pub fn is_running(&self) -> bool {
        self.lock().running
    }

    /// Get the server address if available.
    pub fn get_address(&self) -> Result<String, SyncError> {
        self.lock()
            .address
            .clone()
            .ok_or(SyncError::ServerNotRunning)
    }

    /// Release a startup reservation after startup fails.
    fn startup_failed(&self) {
        let mut inner = self.lock();
        inner.active = false;
        inner.running = false;
        inner.address = None;
        inner.shutdown = None;
    }

    /// Stop the server by triggering shutdown and clearing state.
    /// This combines the commonly used pair: trigger_shutdown + set_stopped.
    pub fn stop_server(&self) {
        let mut inner = self.lock();
        // First trigger shutdown if we have a sender
        if let Some(tx) = inner.shutdown.take() {
            let _ = tx.send(());
        }
        // Then mark as stopped and clear address
        inner.active = false;
        inner.running = false;
        inner.address = None;
    }

    /// Take the lock, recovering from a poisoned one.
    ///
    /// A panic while holding this lock leaves the flags describing a server
    /// that is no longer being driven. Refusing to serve from then on would
    /// turn one panicked task into a permanently dead transport, so the state
    /// is taken as-is and the next start/stop corrects it.
    fn lock(&self) -> std::sync::MutexGuard<'_, ServerStateInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ServerStart<'_> {
    /// Mark startup complete and install the running server state.
    pub(super) fn complete(mut self, address: String, shutdown_sender: oneshot::Sender<()>) {
        let mut inner = self.state.lock();
        debug_assert!(inner.active);
        inner.running = true;
        inner.address = Some(address);
        inner.shutdown = Some(shutdown_sender);
        self.completed = true;
    }
}

impl Drop for ServerStart<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.state.startup_failed();
        }
    }
}

/// Waits for server ready signal and maps errors appropriately.
pub async fn wait_for_ready(
    ready_rx: oneshot::Receiver<()>,
    address: &str,
) -> Result<(), SyncError> {
    ready_rx.await.map_err(|_| SyncError::ServerBind {
        address: address.to_string(),
        reason: "Server startup failed".to_string(),
    })
}
