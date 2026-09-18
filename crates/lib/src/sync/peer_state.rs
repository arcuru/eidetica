//! Per-peer liveness state for the background sync engine.
//!
//! Owned by the [`Sync`](super::Sync) frontend and shared with the engine, so a
//! status read takes the lock directly rather than queueing a command behind an
//! in-flight sync round. The engine is the only writer.

use std::{collections::HashMap, sync::Mutex};

use super::peer_types::PeerId;

/// When each peer last synced, in milliseconds since the Unix epoch.
///
/// Deliberately not persisted. A successful round is a purely observational
/// fact, and writing one entry per peer per round to the sync database would
/// grow the DAG without bound to record it — on the order of a thousand entries
/// a day for a handful of peers at the default interval. The cost is that a
/// peer reads back as never having synced after a restart, which is accurate:
/// the engine genuinely has no record of a round it did not run.
#[derive(Debug, Default)]
pub struct PeerStates {
    last_success_ms: Mutex<HashMap<PeerId, u64>>,
}

impl PeerStates {
    /// When at least one of a peer's trees last synced. `None` until the peer's
    /// first success.
    pub fn last_success_ms(&self, peer_id: &PeerId) -> Option<u64> {
        self.lock().get(peer_id).copied()
    }

    /// Stamp a round in which at least one of the peer's trees synced.
    pub(super) fn record_success(&self, peer_id: &PeerId, now_ms: u64) {
        self.lock().insert(peer_id.clone(), now_ms);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PeerId, u64>> {
        self.last_success_ms
            .lock()
            .expect("peer liveness state mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::PeerStates;
    use crate::{auth::crypto::PrivateKey, sync::peer_types::PeerId};

    fn peer() -> PeerId {
        PeerId::from(&PrivateKey::generate().public_key())
    }

    /// A peer with no successful round on record is reported as such, rather
    /// than as having synced at the epoch.
    #[test]
    fn a_peer_that_has_never_synced_has_no_timestamp() {
        let states = PeerStates::default();
        assert_eq!(states.last_success_ms(&peer()), None);
    }

    /// Each peer is stamped independently: one peer syncing says nothing about
    /// another, which is the whole point of reporting this per peer.
    #[test]
    fn a_recorded_round_is_visible_for_that_peer_alone() {
        let states = PeerStates::default();
        let (synced, other) = (peer(), peer());
        states.record_success(&synced, 1_700_000_000_000);

        assert_eq!(states.last_success_ms(&synced), Some(1_700_000_000_000));
        assert_eq!(states.last_success_ms(&other), None);
    }

    /// The value is "when it last synced", so a later round replaces an earlier
    /// one rather than accumulating.
    #[test]
    fn a_later_round_replaces_the_earlier_timestamp() {
        let states = PeerStates::default();
        let peer = peer();
        states.record_success(&peer, 1_700_000_000_000);
        states.record_success(&peer, 1_700_000_300_000);

        assert_eq!(states.last_success_ms(&peer), Some(1_700_000_300_000));
    }
}
