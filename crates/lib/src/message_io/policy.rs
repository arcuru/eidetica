//! Resource policy and admission accounting for logical wire messages.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use super::TransferError;

const MIB: u64 = 1024 * 1024;

/// Which resource limit refused a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceLimit {
    /// One logical encoded message is larger than `max_message_bytes`.
    MessageBytes,
    /// The budget's admitted message bytes would exceed `max_reserved_bytes`.
    ReservedBytes,
    /// `max_inbound_messages` are already being received or held.
    InboundMessages,
    /// `max_outbound_messages` are already being prepared or sent.
    OutboundMessages,
    /// A decoded message has more values than `max_nodes`.
    Nodes,
    /// A decoded message nests deeper than `max_depth`.
    Depth,
    /// A protocol identifier is longer than `max_identifier_bytes`.
    IdentifierBytes,
    /// A discarded unknown value is longer than `max_ignored_bytes`.
    IgnoredBytes,
}

impl std::fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MessageBytes => "message byte limit",
            Self::ReservedBytes => "reserved byte budget",
            Self::InboundMessages => "active inbound message limit",
            Self::OutboundMessages => "active outbound message limit",
            Self::Nodes => "decoded value limit",
            Self::Depth => "nesting depth limit",
            Self::IdentifierBytes => "identifier byte limit",
            Self::IgnoredBytes => "discarded value byte limit",
        })
    }
}

/// Limits applied to logical messages on the service, Iroh and HTTP wires.
///
/// These are resource-policy limits, not rules about which Entries are valid:
/// a refusal says this endpoint will not carry a message now, under its current
/// configuration. I/O chunk sizes are internal and independent of these values.
///
/// Byte accounting admits encoded message sizes. It is not exact heap
/// accounting and does not bound storage, hashing or caller-owned memory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransferPolicy {
    /// Largest encoded logical message accepted or sent.
    pub max_message_bytes: u64,
    /// Sum of encoded bytes admitted at once by one budget.
    pub max_reserved_bytes: u64,
    /// Inbound messages received or held at once, above the inline size.
    pub max_inbound_messages: usize,
    /// Outbound messages prepared or sent at once, above the inline size.
    pub max_outbound_messages: usize,
    /// Decoded values allowed in one message.
    pub max_nodes: u64,
    /// Nesting allowed in one message.
    pub max_depth: usize,
    /// Encoded bytes allowed for one protocol identifier or discriminant.
    pub max_identifier_bytes: usize,
    /// Encoded bytes allowed for one discarded unknown value.
    pub max_ignored_bytes: usize,
    /// Service response frames queued per connection.
    pub service_queue_slots: usize,
    /// Service write notifications queued per connection, beyond responses.
    ///
    /// A connection that exhausts this is retired rather than losing a
    /// notification, so it must absorb the writes that land while one large
    /// response occupies the connection's writer.
    pub notification_queue_slots: usize,
    /// Wait for the first response byte after a request has been sent.
    pub first_response_timeout: Duration,
    /// Silence tolerated inside a message body.
    pub stall_timeout: Duration,
    /// Length of one fixed progress window.
    pub progress_window: Duration,
    /// Body bytes every complete active window must move.
    pub min_window_bytes: u64,
    /// Caller-selected bound on a whole exchange. Progress never renews it.
    pub overall_timeout: Option<Duration>,
    /// Bound on sizing a message before its first byte is sent.
    pub preparation_timeout: Duration,
}

impl Default for TransferPolicy {
    fn default() -> Self {
        Self {
            max_message_bytes: 256 * MIB,
            max_reserved_bytes: 512 * MIB,
            max_inbound_messages: 4,
            max_outbound_messages: 4,
            max_nodes: 1_048_576,
            max_depth: 128,
            max_identifier_bytes: 4 * 1024,
            max_ignored_bytes: 64 * 1024,
            service_queue_slots: 4,
            notification_queue_slots: 1024,
            first_response_timeout: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(30),
            progress_window: Duration::from_secs(10),
            min_window_bytes: 640 * 1024,
            overall_timeout: None,
            preparation_timeout: Duration::from_secs(30),
        }
    }
}

impl TransferPolicy {
    fn validate(&self) -> Result<(), TransferError> {
        let reject = |reason: &str| Err(TransferError::InvalidPolicy(reason.to_string()));
        if self.max_message_bytes == 0 || self.max_reserved_bytes == 0 {
            return reject("byte limits must be positive");
        }
        if self.max_message_bytes > self.max_reserved_bytes {
            return reject("max_message_bytes exceeds max_reserved_bytes");
        }
        if self.max_inbound_messages == 0 || self.max_outbound_messages == 0 {
            return reject("active message limits must be positive");
        }
        if self.max_nodes == 0 || self.max_depth == 0 {
            return reject("decode limits must be positive");
        }
        if self.max_identifier_bytes == 0 || self.max_ignored_bytes == 0 {
            return reject("decode byte allowances must be positive");
        }
        if self.service_queue_slots == 0
            || self.notification_queue_slots == 0
            || self
                .service_queue_slots
                .checked_add(self.notification_queue_slots)
                .is_none_or(|total| total > tokio::sync::Semaphore::MAX_PERMITS)
        {
            return reject("service queue capacities must be positive and fit a semaphore");
        }
        if self.progress_window.is_zero()
            || self.stall_timeout.is_zero()
            || self.first_response_timeout.is_zero()
            || self.preparation_timeout.is_zero()
            || self.overall_timeout.is_some_and(|limit| limit.is_zero())
        {
            return reject("timeouts must be positive");
        }
        Ok(())
    }
}

/// Current admission use of a [`TransferBudget`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferUsage {
    /// Inbound messages holding a slot.
    pub inbound_messages: usize,
    /// Outbound messages holding a slot.
    pub outbound_messages: usize,
    /// Encoded bytes admitted.
    pub reserved_bytes: u64,
}

struct BudgetInner {
    policy: TransferPolicy,
    usage: Mutex<TransferUsage>,
}

impl BudgetInner {
    /// Counters stay meaningful across a panic, so poisoning is not a refusal.
    fn usage(&self) -> MutexGuard<'_, TransferUsage> {
        self.usage.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Shared admission state for the messages of one or more endpoints.
///
/// A constructed client, server or transport shares one budget across all of
/// its connections. Clone a budget into several endpoints to share a limit
/// between them; independent endpoints have independent budgets, so this is not
/// a process-wide memory ceiling.
#[derive(Clone)]
pub struct TransferBudget {
    inner: Arc<BudgetInner>,
}

impl std::fmt::Debug for TransferBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferBudget")
            .field("policy", &self.inner.policy)
            .field("usage", &self.usage())
            .finish()
    }
}

impl Default for TransferBudget {
    fn default() -> Self {
        Self {
            inner: Arc::new(BudgetInner {
                policy: TransferPolicy::default(),
                usage: Mutex::default(),
            }),
        }
    }
}

impl TransferBudget {
    /// Create a budget, rejecting an inconsistent policy.
    pub fn new(policy: TransferPolicy) -> crate::Result<Self> {
        policy.validate()?;
        Ok(Self {
            inner: Arc::new(BudgetInner {
                policy,
                usage: Mutex::default(),
            }),
        })
    }

    /// The policy this budget enforces.
    pub fn policy(&self) -> &TransferPolicy {
        &self.inner.policy
    }

    /// Admission use at this moment.
    pub fn usage(&self) -> TransferUsage {
        *self.inner.usage()
    }

    /// Reserve bytes without taking a blocking-worker slot.
    pub(crate) fn try_inline(&self, bytes: u64) -> Result<Lease, TransferError> {
        let mut lease = Lease {
            budget: self.inner.clone(),
            direction: None,
            bytes: 0,
        };
        lease.reserve(bytes)?;
        Ok(lease)
    }

    /// Take an inbound slot, failing fast so waits cannot form a cycle.
    pub(crate) fn try_inbound(&self) -> Result<Lease, TransferError> {
        self.try_slot(Direction::Inbound)
    }

    /// Take an outbound slot, failing fast so waits cannot form a cycle.
    pub(crate) fn try_outbound(&self) -> Result<Lease, TransferError> {
        self.try_slot(Direction::Outbound)
    }

    fn try_slot(&self, direction: Direction) -> Result<Lease, TransferError> {
        let policy = &self.inner.policy;
        let mut usage = self.inner.usage();
        let (active, limit, refusal) = match direction {
            Direction::Inbound => (
                &mut usage.inbound_messages,
                policy.max_inbound_messages,
                ResourceLimit::InboundMessages,
            ),
            Direction::Outbound => (
                &mut usage.outbound_messages,
                policy.max_outbound_messages,
                ResourceLimit::OutboundMessages,
            ),
        };
        if *active >= limit {
            return Err(TransferError::resource(refusal, format!("limit {limit}")));
        }
        *active += 1;
        Ok(Lease {
            budget: self.inner.clone(),
            direction: Some(direction),
            bytes: 0,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Inbound,
    Outbound,
}

/// One message's admission. Releases its slot and bytes when dropped, so it
/// must live as long as the worker or data it accounts for.
pub(crate) struct Lease {
    budget: Arc<BudgetInner>,
    direction: Option<Direction>,
    bytes: u64,
}

impl Lease {
    /// Change this lease's reservation to exactly `bytes`.
    pub(crate) fn reserve(&mut self, bytes: u64) -> Result<(), TransferError> {
        let policy = &self.budget.policy;
        if bytes > policy.max_message_bytes {
            return Err(TransferError::resource(
                ResourceLimit::MessageBytes,
                format!("{bytes} bytes exceeds {}", policy.max_message_bytes),
            ));
        }
        let mut usage = self.budget.usage();
        let others = usage.reserved_bytes - self.bytes;
        let total = others
            .checked_add(bytes)
            .filter(|total| *total <= policy.max_reserved_bytes);
        let Some(total) = total else {
            return Err(TransferError::resource(
                ResourceLimit::ReservedBytes,
                format!("{bytes} bytes with {others} already reserved"),
            ));
        };
        usage.reserved_bytes = total;
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut usage = self.budget.usage();
        usage.reserved_bytes -= self.bytes;
        match self.direction {
            Some(Direction::Inbound) => usage.inbound_messages -= 1,
            Some(Direction::Outbound) => usage.outbound_messages -= 1,
            None => {}
        }
    }
}
