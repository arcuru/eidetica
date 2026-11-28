//! Local due-times for automatic reconciliation. The queue has at most one
//! deadline per peer, never a timer or a backlog of missed ticks per pair.

use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    time::Duration,
};

use rand::{Rng, rngs::StdRng};
use tokio::time::Instant;
use tracing::{debug, trace, warn};

use super::RoundReport;
use crate::{entry::ID, sync::peer_types::PeerId};

pub(super) const DEFAULT_INTERVAL: u64 = 300;
pub(super) const SETTINGS_CHECK: Duration = Duration::from_secs(60);
const MIN_PERIOD: u64 = 5;
const MAX_PERIOD: u64 = 3600;
const SPACING: Duration = Duration::from_secs(1);
const TOKEN: Duration = Duration::from_secs(1);
const BURST: Duration = Duration::from_secs(32);

pub(super) type Periods = HashMap<ID, u64>;

struct Pair {
    configured: u64,
    due: Instant,
    completed: Option<Instant>,
    jitter: f64,
    last_attempt_ms: Option<u64>,
    last_success_ms: Option<u64>,
}

impl Pair {
    fn period(&self) -> u64 {
        self.configured.clamp(MIN_PERIOD, MAX_PERIOD)
    }

    fn configure(&mut self, configured: u64, peer: &PeerId, tree: &ID) {
        if configured < MIN_PERIOD {
            warn!(%peer, %tree, configured, minimum = MIN_PERIOD, "Clamping automatic sync interval");
        }
        self.configured = configured;
        if let Some(completed) = self.completed {
            // Shorter intervals can pull work forward; settings changes never
            // postpone already scheduled work, even under repeated changes.
            self.due = self.due.min(completed + self.delay());
        }
    }

    fn delay(&self) -> Duration {
        Duration::from_secs(self.period()).mul_f64(self.jitter)
    }
}

struct Peer {
    pairs: HashMap<ID, Pair>,
    in_flight: bool,
    not_before: Instant,
    failures: u32,
    // The ordered index uses the same key for insertion and removal. A peer
    // can change its deadline repeatedly without accumulating stale entries.
    key: String,
    queued_at: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Budget {
    Concurrency,
    Rate,
}

pub(super) struct Schedule {
    peers: HashMap<PeerId, Peer>,
    ready: BTreeMap<(Instant, String), PeerId>,
    rng: StdRng,
    initialized: bool,
    // At one start per second, a duration can represent fractional credit
    // exactly. Clamping before spending also discards time spent at capacity.
    credit: Duration,
    accounted_at: Instant,
    limited: Option<Budget>,
    concurrency_deferrals: u64,
    rate_deferrals: u64,
    spacing_deferrals: u64,
}

impl Schedule {
    pub(super) fn new(now: Instant, rng: StdRng) -> Self {
        Self {
            peers: HashMap::new(),
            ready: BTreeMap::new(),
            rng,
            initialized: false,
            credit: BURST,
            accounted_at: now,
            limited: None,
            concurrency_deferrals: 0,
            rate_deferrals: 0,
            spacing_deferrals: 0,
        }
    }

    pub(super) fn refresh(&mut self, mut snapshot: HashMap<PeerId, Periods>, now: Instant) {
        let removed: Vec<_> = self
            .peers
            .keys()
            .filter(|peer| !snapshot.contains_key(*peer))
            .cloned()
            .collect();
        for peer in removed {
            snapshot.insert(peer, Periods::new());
        }
        for (peer, periods) in snapshot {
            self.update_peer(&peer, periods, now);
        }
        self.initialized = true;
    }

    pub(super) fn update_peer(&mut self, id: &PeerId, periods: Periods, now: Instant) {
        if periods.is_empty() && !self.peers.contains_key(id) {
            return;
        }
        let peer = self.peers.entry(id.clone()).or_insert_with(|| Peer {
            pairs: HashMap::new(),
            in_flight: false,
            not_before: now,
            failures: 0,
            key: id.to_string(),
            queued_at: None,
        });
        let new_due = if self.initialized {
            now
        } else {
            now + Duration::from_millis(self.rng.gen_range(0..=SETTINGS_CHECK.as_millis() as u64))
        };
        peer.pairs.retain(|tree, _| periods.contains_key(tree));
        for (tree, configured) in periods {
            match peer.pairs.entry(tree.clone()) {
                Entry::Vacant(entry) => {
                    let mut pair = Pair {
                        configured,
                        due: new_due,
                        completed: None,
                        jitter: 1.0,
                        last_attempt_ms: None,
                        last_success_ms: None,
                    };
                    pair.configure(configured, id, &tree);
                    entry.insert(pair);
                }
                Entry::Occupied(mut entry) if entry.get().configured != configured => {
                    entry.get_mut().configure(configured, id, &tree);
                }
                Entry::Occupied(_) => {}
            }
        }
        self.requeue(id);
    }

    fn requeue(&mut self, id: &PeerId) {
        let peer = self.peers.get_mut(id).expect("scheduled peer");
        if let Some(old) = peer.queued_at.take() {
            self.ready.remove(&(old, peer.key.clone()));
        }
        if peer.in_flight {
            return;
        }
        if let Some(due) = peer.pairs.values().map(|pair| pair.due).min() {
            let ready = due.max(peer.not_before);
            self.ready.insert((ready, peer.key.clone()), id.clone());
            peer.queued_at = Some(ready);
        } else {
            self.peers.remove(id);
        }
    }

    fn token_at(&self) -> Instant {
        self.accounted_at + TOKEN.saturating_sub(self.credit)
    }

    pub(super) fn wake_at(&mut self, now: Instant, at_capacity: bool) -> Option<Instant> {
        let deadline = self.ready.first_key_value().map(|((due, _), _)| *due);
        let token = self.token_at();
        let limited = deadline.filter(|due| *due <= now).and_then(|_| {
            if at_capacity {
                Some(Budget::Concurrency)
            } else if token > now {
                Some(Budget::Rate)
            } else {
                None
            }
        });
        if limited != self.limited {
            self.limited = limited;
            match limited {
                Some(Budget::Concurrency) => self.concurrency_deferrals += 1,
                Some(Budget::Rate) => self.rate_deferrals += 1,
                None => {}
            }
            if limited.is_some() {
                trace!(
                    concurrency = self.concurrency_deferrals,
                    rate = self.rate_deferrals,
                    "Automatic sync start deferred by budget"
                );
            }
        }
        if at_capacity {
            None // A completion, not a timer, can open a slot.
        } else {
            deadline.map(|due| due.max(token))
        }
    }

    pub(super) fn due_peer(&self, now: Instant) -> Option<&PeerId> {
        self.ready
            .first_key_value()
            .filter(|((due, _), _)| *due <= now && self.token_at() <= now)
            .map(|(_, peer)| peer)
    }

    pub(super) fn start(&mut self, id: &PeerId, now: Instant) {
        debug_assert_eq!(self.due_peer(now), Some(id));
        self.credit = (self.credit + now.duration_since(self.accounted_at)).min(BURST) - TOKEN;
        self.accounted_at = now;
        self.peers.get_mut(id).expect("due peer").in_flight = true;
        self.requeue(id);
    }

    /// A local policy read failed. Keep the schedule, but don't dial using
    /// potentially stale membership or opt-out state.
    pub(super) fn defer_refresh(&mut self, id: &PeerId, now: Instant) {
        self.peers.get_mut(id).expect("due peer").not_before = now + SETTINGS_CHECK;
        self.requeue(id);
    }

    pub(super) fn finish(&mut self, id: &PeerId, completed: Instant, report: RoundReport) {
        let peer = self.peers.get_mut(id).expect("in-flight peer");
        peer.in_flight = false;
        peer.not_before = completed + SPACING;
        let interrupted = report.network_failure;
        for result in report.trees {
            if let Some(pair) = peer.pairs.get_mut(&result.tree) {
                pair.last_attempt_ms = result.attempted_ms;
                if let Some(success_ms) = result.success_ms {
                    pair.last_success_ms = Some(success_ms);
                    pair.completed = Some(completed);
                    pair.jitter = self.rng.gen_range(0.75..=1.25);
                    pair.due = completed + pair.delay();
                }
                trace!(peer = %id, tree = %result.tree, attempt_ms = ?pair.last_attempt_ms,
                    success_ms = ?pair.last_success_ms, next_due = ?pair.due, "Automatic tree sync schedule");
            }
        }
        if report.reached_peer {
            peer.failures = 0;
        }
        if interrupted && let Some(shortest) = peer.pairs.values().map(Pair::period).min() {
            peer.failures = (peer.failures + 1).min(MAX_PERIOD.ilog2() + 1);
            let base = (shortest * (1 << peer.failures)).min(MAX_PERIOD);
            let delay = Duration::from_secs(base).mul_f64(self.rng.gen_range(0.75..=1.25));
            peer.not_before = completed + delay.max(SPACING);
        } else if peer.pairs.values().any(|pair| pair.due < peer.not_before) {
            self.spacing_deferrals += 1;
            trace!(
                count = self.spacing_deferrals,
                "Automatic sync start deferred by peer spacing"
            );
        }
        debug!(peer = %id, backoff_level = peer.failures, next_start = ?peer.pairs.values()
            .map(|pair| pair.due.max(peer.not_before)).min(), "Automatic peer sync schedule");
        self.requeue(id);
    }
}

#[cfg(test)]
mod tests;
