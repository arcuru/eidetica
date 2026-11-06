//! Progress-aware timing for message bodies.
//!
//! A healthy transfer may take as long as it needs, so a body has no total
//! deadline of its own. It must instead keep moving: silence for
//! `stall_timeout` fails it, and so does any active window that moves fewer than
//! `min_window_bytes`. Windows open on the first byte after the last one closed,
//! carry no credit forward, and do not apply to a body that finishes inside one.
//!
//! Bytes are counted when the native transport accepts or yields them. That is
//! acceptance by the transport, not proof a packet was acknowledged.

use std::{sync::Mutex, time::Duration};

use tokio::{sync::Notify, time::Instant};

use super::{TransferError, TransferPolicy};

#[derive(Debug, Clone, Copy)]
struct Window {
    ends: Instant,
    bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// The body is moving.
    Body {
        last_progress: Instant,
        window: Option<Window>,
    },
    /// The body has finished and a response is due by this time.
    AwaitingResponse { deadline: Instant },
    /// Nothing further is timed by this monitor.
    Finished,
}

/// Times one direction of one message. Shared between the task moving bytes and
/// the task enforcing the deadlines.
pub(crate) struct ProgressMonitor {
    stall_timeout: Duration,
    progress_window: Duration,
    min_window_bytes: u64,
    first_response_timeout: Duration,
    phase: Mutex<Phase>,
    changed: Notify,
}

impl ProgressMonitor {
    pub(crate) fn new(policy: &TransferPolicy) -> Self {
        Self {
            stall_timeout: policy.stall_timeout,
            progress_window: policy.progress_window,
            min_window_bytes: policy.min_window_bytes,
            first_response_timeout: policy.first_response_timeout,
            phase: Mutex::new(Phase::Body {
                last_progress: Instant::now(),
                window: None,
            }),
            changed: Notify::new(),
        }
    }

    fn phase(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record body bytes the native transport accepted or delivered.
    pub(crate) fn record(&self, bytes: usize) -> Result<(), TransferError> {
        if bytes == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut phase = self.phase();
        self.check(&mut phase, now)?;
        let Phase::Body {
            last_progress,
            window,
        } = &mut *phase
        else {
            return Ok(());
        };
        *last_progress = now;
        match window {
            Some(window) => window.bytes = window.bytes.saturating_add(bytes as u64),
            None => {
                *window = Some(Window {
                    ends: now + self.progress_window,
                    bytes: bytes as u64,
                });
                // A window can end before the watcher's current deadline.
                self.changed.notify_one();
            }
        }
        Ok(())
    }

    /// The body is complete; rate windows stop here. A caller expecting an
    /// answer starts the first-response wait, any other caller stops timing.
    pub(crate) fn body_complete(&self, awaiting_response: bool) {
        *self.phase() = if awaiting_response {
            Phase::AwaitingResponse {
                deadline: Instant::now() + self.first_response_timeout,
            }
        } else {
            Phase::Finished
        };
        self.changed.notify_one();
    }

    /// Whether the native body consumer has taken the complete upload.
    pub(crate) fn is_body_complete(&self) -> bool {
        !matches!(*self.phase(), Phase::Body { .. })
    }

    /// The first response byte arrived, so the first-response wait is over.
    /// An upload still in progress keeps being timed.
    pub(crate) fn response_started(&self) {
        let mut phase = self.phase();
        if matches!(*phase, Phase::AwaitingResponse { .. }) {
            *phase = Phase::Finished;
            self.changed.notify_one();
        }
    }

    /// Fail a body that has fallen behind, and close windows that are over.
    fn check(&self, phase: &mut Phase, now: Instant) -> Result<(), TransferError> {
        match phase {
            Phase::Body {
                last_progress,
                window,
            } => {
                if let Some(open) = *window
                    && now >= open.ends
                {
                    if open.bytes < self.min_window_bytes {
                        return Err(TransferError::InsufficientProgress {
                            bytes: open.bytes,
                            window: self.progress_window,
                        });
                    }
                    *window = None;
                }
                if now.duration_since(*last_progress) >= self.stall_timeout {
                    return Err(TransferError::Stalled {
                        elapsed: self.stall_timeout,
                    });
                }
                Ok(())
            }
            Phase::AwaitingResponse { deadline } if now >= *deadline => {
                Err(TransferError::ResponseTimeout {
                    elapsed: self.first_response_timeout,
                })
            }
            Phase::AwaitingResponse { .. } | Phase::Finished => Ok(()),
        }
    }

    fn next_deadline(&self, phase: &Phase) -> Option<Instant> {
        match phase {
            Phase::Body {
                last_progress,
                window,
            } => {
                let stall = *last_progress + self.stall_timeout;
                Some(window.map_or(stall, |open| open.ends.min(stall)))
            }
            Phase::AwaitingResponse { deadline } => Some(*deadline),
            Phase::Finished => None,
        }
    }

    /// Resolve with the failure once this body has fallen behind.
    pub(crate) async fn expired(&self) -> TransferError {
        loop {
            let deadline = {
                let mut phase = self.phase();
                if let Err(failure) = self.check(&mut phase, Instant::now()) {
                    return failure;
                }
                self.next_deadline(&phase)
            };
            match deadline {
                Some(deadline) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(deadline) => {}
                        () = self.changed.notified() => {}
                    }
                }
                None => self.changed.notified().await,
            }
        }
    }

    /// Run `work` until it finishes or this body falls behind.
    pub(crate) async fn run<F, T>(&self, work: F) -> Result<T, TransferError>
    where
        F: Future<Output = Result<T, TransferError>>,
    {
        tokio::select! {
            result = work => result,
            failure = self.expired() => Err(failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: usize = 1024;

    async fn advance(seconds: u64) {
        tokio::time::advance(Duration::from_secs(seconds)).await;
    }

    fn monitor() -> ProgressMonitor {
        ProgressMonitor::new(&TransferPolicy::default())
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_progress_outlives_every_single_timeout() {
        let monitor = monitor();
        for _ in 0..30 {
            advance(5).await;
            monitor.record(400 * KIB).unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn silence_fails_at_the_stall_interval_not_a_window() {
        let monitor = monitor();
        let started = Instant::now();
        let failure = monitor.expired().await;
        assert!(
            matches!(failure, TransferError::Stalled { .. }),
            "{failure}"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn slow_trickle_fails_though_it_is_never_silent() {
        let monitor = monitor();
        monitor.record(1).unwrap();
        let started = Instant::now();
        let failure = monitor
            .run::<_, ()>(async {
                loop {
                    advance(1).await;
                    monitor.record(1)?;
                }
            })
            .await
            .unwrap_err();
        assert!(
            matches!(failure, TransferError::InsufficientProgress { .. }),
            "{failure}"
        );
        assert!(started.elapsed() <= Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn an_early_burst_does_not_prepay_later_windows() {
        let monitor = monitor();
        monitor.record(100 * 640 * KIB).unwrap();
        advance(11).await;
        monitor.record(1).unwrap();
        advance(10).await;
        assert!(matches!(
            monitor.record(1),
            Err(TransferError::InsufficientProgress { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_short_body_does_not_have_to_fill_a_window() {
        let monitor = monitor();
        monitor.record(10).unwrap();
        advance(2).await;
        monitor.body_complete(false);
        advance(3600).await;
        monitor.record(10).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn response_wait_is_a_separate_phase() {
        let monitor = monitor();
        monitor.record(10).unwrap();
        monitor.body_complete(true);
        let started = Instant::now();
        let failure = monitor.expired().await;
        assert!(
            matches!(failure, TransferError::ResponseTimeout { .. }),
            "{failure}"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }
}
