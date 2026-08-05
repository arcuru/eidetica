//! Bootstrap request management for the sync module.
//!
//! This module handles storing and managing bootstrap requests that require manual approval.
//! Bootstrap requests are stored in the sync database as an Instance-level concern.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use super::peer_types::Address;
use crate::{
    Error, Result, Transaction,
    auth::{Permission, crypto::PublicKey},
    crdt::Doc,
    entry::ID,
    store::{StoreError, Table},
};

/// Private constant for bootstrap request subtree name
pub(super) const BOOTSTRAP_REQUESTS_SUBTREE: &str = "bootstrap_requests";

/// Internal bootstrap request manager for the sync module.
///
/// This struct manages all bootstrap request operations for the sync module,
/// operating on a Transaction to stage changes.
pub(super) struct BootstrapRequestManager<'a> {
    txn: &'a Transaction,
}

/// A bootstrap request awaiting manual approval
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootstrapRequest {
    /// The tree ID being requested for access
    pub tree_id: ID,
    /// Public key of the requesting device
    pub requesting_pubkey: PublicKey,
    /// Key name identifier for the requesting key
    pub requesting_key_name: String,
    /// Permission level being requested
    pub requested_permission: Permission,
    /// When the request was made (ISO 8601 timestamp)
    pub timestamp: String,
    /// Current status of the request
    pub status: RequestStatus,
    /// Address of the requesting peer (for future notifications)
    pub peer_address: Address,
    /// Free-form context supplied by the requester for the approver to inspect
    /// when deciding whether to grant access. Carried verbatim from the request.
    #[serde(default)]
    pub metadata: Option<Doc>,
    /// The requester's handshake device key, if the request arrived over an
    /// authenticated connection.
    ///
    /// Recorded so approval can add the requester to the database's tree-peer set
    /// and reach it with the approval broadcast. Registration deliberately does
    /// *not* happen when the request is made: that set is the push list, and an
    /// unapproved peer must not be on it. `None` means the approval broadcast has
    /// no route to the requester, which is not fatal — the requester's own
    /// completion sweep still converges.
    #[serde(default)]
    pub peer_device_pubkey: Option<PublicKey>,
}

/// Status of a bootstrap request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RequestStatus {
    /// Request is pending approval
    Pending,
    /// Request has been approved
    Approved {
        /// Who approved the request
        approved_by: String,
        /// When it was approved
        approval_time: String,
    },
    /// Request has been rejected
    Rejected {
        /// Who rejected the request
        rejected_by: String,
        /// When it was rejected
        rejection_time: String,
    },
}

/// Namespace for deriving bootstrap request ids.
///
/// A fixed arbitrary UUID. Its only job is to keep derived ids from colliding
/// with name-based ids derived elsewhere for a different purpose.
const BOOTSTRAP_REQUEST_NAMESPACE: Uuid = Uuid::from_bytes([
    0x8f, 0x3d, 0x1a, 0x77, 0x2c, 0x94, 0x4e, 0x5b, 0xa1, 0x60, 0xd7, 0xe8, 0x35, 0x0b, 0x9c, 0x42,
]);

/// Derive the storage id for a bootstrap request from what it asks for.
///
/// A request is identified by the ask — `(tree, requesting key, permission)` —
/// not by when it arrived, so the same ask always addresses the same row.
///
/// This is what keeps the queue free of duplicates under concurrency. A ticket
/// carrying several address hints dials them all at once (that race is how a
/// ticket with both a LAN and a relay address connects fast), so several racers
/// can run a full bootstrap round-trip against the same owner. Answering from an
/// existing record is not enough on its own: racers that all look before any of
/// them commits each see an empty queue. Deriving the key from the ask makes the
/// duplicate impossible instead of merely unlikely — every racer writes the same
/// key, and the merge yields one row. The same argument covers requests that
/// arrive at genuinely separate replicas and meet later during sync.
///
/// The permission is part of the id on purpose: a retry re-sends the same one,
/// but asking for `Admin` after a pending `Read` is a different ask, and
/// collapsing those would let approving the weaker record answer the escalation.
///
/// Ids are stable across restarts and across peers, which also means an operator
/// can be handed the same id twice for the same ask without it meaning anything
/// went wrong.
pub(super) fn request_id_for(
    tree_id: &ID,
    requesting_pubkey: &PublicKey,
    requested_permission: &Permission,
) -> String {
    // Spelled out rather than derived from `Debug` or the serde form, so a
    // change to either does not silently repartition the queue.
    let permission = match requested_permission {
        Permission::Admin(priority) => format!("admin:{priority}"),
        Permission::Write(priority) => format!("write:{priority}"),
        Permission::Read => "read".to_string(),
    };
    // Unit separator: none of the three components can contain it, so the
    // joined form is unambiguous.
    let name = format!("{tree_id}\u{1f}{requesting_pubkey}\u{1f}{permission}");
    Uuid::new_v5(&BOOTSTRAP_REQUEST_NAMESPACE, name.as_bytes()).to_string()
}

impl<'a> BootstrapRequestManager<'a> {
    /// Create a new BootstrapRequestManager that operates on the given Transaction.
    pub(super) fn new(txn: &'a Transaction) -> Self {
        Self { txn }
    }

    /// Store a bootstrap request in the sync database under its derived id.
    ///
    /// The id is a pure function of `(tree_id, requesting_pubkey,
    /// requested_permission)` — see [`request_id_for`] — so storing the same ask
    /// twice writes the same row rather than appending a second one. Two writers
    /// that never saw each other's record still converge on one entry, because
    /// they address the same key and the CRDT merge collapses them.
    ///
    /// # Arguments
    /// * `request` - The bootstrap request to store
    ///
    /// # Returns
    /// The derived id for the request.
    pub(super) async fn store_request(&self, request: BootstrapRequest) -> Result<String> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        debug!(tree_id = %request.tree_id, "Storing bootstrap request");

        let request_id = request_id_for(
            &request.tree_id,
            &request.requesting_pubkey,
            &request.requested_permission,
        );
        requests.set(&request_id, request.clone()).await?;

        info!(request_id = %request_id, tree_id = %request.tree_id, "Successfully stored bootstrap request");
        Ok(request_id)
    }

    /// Get a specific bootstrap request by ID.
    ///
    /// # Arguments
    /// * `request_id` - The ID of the request to retrieve
    ///
    /// # Returns
    /// The bootstrap request if found, None otherwise.
    pub(super) async fn get_request(&self, request_id: &str) -> Result<Option<BootstrapRequest>> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        match requests.get(request_id).await {
            Ok(request) => Ok(Some(request)),
            Err(Error::Store(ref e)) if matches!(**e, StoreError::KeyNotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Internal method to filter bootstrap requests by status.
    async fn filter_requests(
        &self,
        status_filter: &RequestStatus,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        let results = requests
            .search(|request| {
                std::mem::discriminant(status_filter) == std::mem::discriminant(&request.status)
            })
            .await?;

        Ok(results)
    }

    /// Get all pending bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for pending requests.
    pub(super) async fn pending_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Pending).await
    }

    /// Get all approved bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for approved requests.
    pub(super) async fn approved_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Approved {
            approved_by: String::new(),
            approval_time: String::new(),
        })
        .await
    }

    /// Get all rejected bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for rejected requests.
    pub(super) async fn rejected_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Rejected {
            rejected_by: String::new(),
            rejection_time: String::new(),
        })
        .await
    }

    /// Find an existing request matching `(tree_id, requesting_pubkey, permission)`
    /// that is still an answer — i.e. `Pending` or `Rejected`.
    ///
    /// Answers a re-request from the record already on file, so a requester that
    /// re-sends (the outgoing-bootstrap sweep does so on every tick until access
    /// is granted) learns it has been rejected rather than being told to keep
    /// waiting. Keeping the queue itself free of duplicates is the job of the
    /// derived request id — see [`request_id_for`] — which holds even for writers
    /// that reach this lookup before any of them has committed.
    ///
    /// The permission is part of the match, for the same reason it is part of the
    /// id: asking for `Admin` after a pending `Read` is a materially different
    /// request, and collapsing the two would answer the escalation with the
    /// weaker record's id — approving it would then grant less than was asked
    /// for, silently.
    ///
    /// An `Approved` record is deliberately ignored: reaching the store path at
    /// all means the auth check found no live grant, so the approval was since
    /// revoked and a genuinely new request is the right outcome.
    ///
    /// Bounding an *honest* client is not a defence against a hostile peer
    /// cycling permission values to force new rows. That needs a per-peer cap on
    /// pending requests — see the `TODO(bootstrap-metadata-bound)` note in
    /// `handler.rs`.
    ///
    /// # Returns
    /// The (request_id, request) of the existing record, or `None`.
    pub(super) async fn find_existing_request(
        &self,
        tree_id: &ID,
        requesting_pubkey: &PublicKey,
        requested_permission: &Permission,
    ) -> Result<Option<(String, BootstrapRequest)>> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        let mut matches = requests
            .search(|request| {
                &request.tree_id == tree_id
                    && &request.requesting_pubkey == requesting_pubkey
                    && &request.requested_permission == requested_permission
                    && matches!(
                        request.status,
                        RequestStatus::Pending | RequestStatus::Rejected { .. }
                    )
            })
            .await?;

        // A rejection is the more decisive answer, so prefer it if both exist
        // (possible if a request was rejected and a later one is still pending).
        if let Some(idx) = matches
            .iter()
            .position(|(_, r)| matches!(r.status, RequestStatus::Rejected { .. }))
        {
            return Ok(Some(matches.swap_remove(idx)));
        }
        Ok(matches.into_iter().next())
    }

    /// Update the status of a bootstrap request.
    ///
    /// # Arguments
    /// * `request_id` - The ID of the request to update
    /// * `new_status` - The new status to set
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn update_status(
        &self,
        request_id: &str,
        new_status: RequestStatus,
    ) -> Result<()> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        // Get the existing request
        let mut request = requests.get(request_id).await?;

        // Update the status
        request.status = new_status;

        // Store the updated request
        requests.set(request_id, request).await?;

        debug!(request_id = %request_id, "Updated bootstrap request status");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Clock, Database, Instance, auth::types::Permission, backend::database::InMemory,
        clock::FixedClock, crdt::Doc,
    };
    use std::sync::Arc;

    async fn create_test_sync_tree() -> (Instance, Database, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::default());
        let (instance, mut user) = Instance::create_backend_with_clock(
            Box::new(InMemory::new()),
            clock.clone(),
            crate::NewUser::passwordless("test"),
        )
        .await
        .expect("Failed to create test instance");

        let mut sync_settings = Doc::new();
        sync_settings.set("name", "_sync");
        sync_settings.set("type", "sync_settings");

        let (database, _) = user
            .new_database()
            .settings(sync_settings)
            .build()
            .await
            .unwrap();

        (instance, database, clock)
    }

    fn create_test_request(clock: &FixedClock) -> BootstrapRequest {
        BootstrapRequest {
            // Use a valid, prefixed ID so parsing validates correctly
            tree_id: ID::from_bytes("test_tree_id"),
            requesting_pubkey: PublicKey::random(),
            requesting_key_name: "laptop_key".to_string(),
            requested_permission: Permission::Write(5),
            timestamp: clock.now_rfc3339(),
            status: RequestStatus::Pending,
            peer_address: Address {
                transport_type: "http".to_string(),
                address: "127.0.0.1:8080".to_string(),
            },
            metadata: None,
            peer_device_pubkey: Some(PublicKey::random()),
        }
    }

    #[tokio::test]
    async fn test_store_and_get_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);

        // Store the request and get the generated UUID
        let request_id = manager.store_request(request.clone()).await.unwrap();

        // Retrieve the request
        let retrieved = manager.get_request(&request_id).await.unwrap().unwrap();
        assert_eq!(retrieved.tree_id, request.tree_id);
        assert_eq!(retrieved.requesting_pubkey, request.requesting_pubkey);
        assert_eq!(retrieved.requesting_key_name, request.requesting_key_name);
        assert_eq!(retrieved.requested_permission, request.requested_permission);
        assert_eq!(retrieved.status, request.status);
        assert_eq!(retrieved.peer_address, request.peer_address);
    }

    #[tokio::test]
    async fn test_list_requests() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        // Store multiple requests
        let request1 = create_test_request(&clock);

        let mut request2 = create_test_request(&clock);
        request2.status = RequestStatus::Approved {
            approved_by: "admin".to_string(),
            approval_time: clock.now_rfc3339(),
        };

        manager.store_request(request1).await.unwrap();
        manager.store_request(request2).await.unwrap();

        // Get pending requests
        let pending_requests = manager.pending_requests().await.unwrap();
        assert_eq!(pending_requests.len(), 1);

        // Get approved requests
        let approved_requests = manager.approved_requests().await.unwrap();
        assert_eq!(approved_requests.len(), 1);

        // Verify statuses
        assert!(matches!(
            pending_requests[0].1.status,
            RequestStatus::Pending
        ));
        assert!(matches!(
            approved_requests[0].1.status,
            RequestStatus::Approved { .. }
        ));
    }

    #[tokio::test]
    async fn test_update_status() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);

        // Store the request and get the generated UUID
        let request_id = manager.store_request(request).await.unwrap();

        // Update status to approved
        let new_status = RequestStatus::Approved {
            approved_by: "admin".to_string(),
            approval_time: clock.now_rfc3339(),
        };
        manager
            .update_status(&request_id, new_status.clone())
            .await
            .unwrap();

        // Verify status was updated
        let updated_request = manager.get_request(&request_id).await.unwrap().unwrap();
        assert_eq!(updated_request.status, new_status);
    }

    /// Two writers that both look before either commits must still leave one
    /// row. This is the interleaving racing address hints produce: each racer
    /// runs its own bootstrap round-trip, and answering from an existing record
    /// cannot help a racer that looked at an empty queue.
    ///
    /// Written as an explicit interleaving rather than a timing race — the
    /// window between the lookup and the commit is sub-millisecond, so a
    /// wall-clock race reproduces it only occasionally and would be a flaky
    /// guard against regression.
    #[tokio::test]
    async fn concurrent_writers_converge_on_one_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let request = create_test_request(&clock);

        // Seed an unrelated request so the store exists before the two writers
        // fork. Racers that also create the store itself concurrently root two
        // disjoint store histories, which is a defect in the subtree merge
        // rather than in this queue — see the fix that merges disjoint store
        // histories from the empty base.
        let seed = sync_tree.new_transaction().await.unwrap();
        let seed_id = BootstrapRequestManager::new(&seed)
            .store_request(create_test_request(&clock))
            .await
            .unwrap();
        seed.commit().await.unwrap();

        // Both transactions open before either commits, so neither sees the
        // other's lookup — the same position two racing hints reach.
        let first = sync_tree.new_transaction().await.unwrap();
        let second = sync_tree.new_transaction().await.unwrap();

        for txn in [&first, &second] {
            let existing = BootstrapRequestManager::new(txn)
                .find_existing_request(
                    &request.tree_id,
                    &request.requesting_pubkey,
                    &request.requested_permission,
                )
                .await
                .unwrap();
            assert!(
                existing.is_none(),
                "neither writer can see a record that has not been committed yet"
            );
        }

        let first_id = BootstrapRequestManager::new(&first)
            .store_request(request.clone())
            .await
            .unwrap();
        first.commit().await.unwrap();

        // The racers arrive over different hints at different moments, so the
        // records are not byte-identical. Vary them, or the two entries would be
        // one entry by content address and the merge would never be exercised.
        let mut later = request.clone();
        later.timestamp = "2026-08-04T12:00:01Z".to_string();
        later.peer_address = Address {
            transport_type: "http".to_string(),
            address: "127.0.0.2:8080".to_string(),
        };

        let second_id = BootstrapRequestManager::new(&second)
            .store_request(later)
            .await
            .unwrap();
        second.commit().await.unwrap();

        assert_eq!(
            first_id, second_id,
            "the same ask derives the same id, so both writers address one row"
        );

        let txn = sync_tree.new_transaction().await.unwrap();
        let pending = BootstrapRequestManager::new(&txn)
            .pending_requests()
            .await
            .unwrap();
        assert_eq!(
            pending.iter().filter(|(id, _)| *id == first_id).count(),
            1,
            "the racing writers leave one row between them, found: {pending:#?}"
        );
        assert_eq!(
            pending.len(),
            2,
            "and the unrelated seeded request is untouched, found: {pending:#?}"
        );
        assert!(pending.iter().any(|(id, _)| *id == seed_id));
    }

    /// The id is a function of the ask, and a different ask is a different row.
    /// Permission is part of the ask on purpose: an escalation must not be
    /// answered by approving the weaker pending record.
    #[tokio::test]
    async fn request_id_is_derived_from_the_ask() {
        let tree = ID::from_bytes("test_tree_id");
        let key = PublicKey::random();

        assert_eq!(
            request_id_for(&tree, &key, &Permission::Write(5)),
            request_id_for(&tree, &key, &Permission::Write(5)),
            "the same ask is stable across calls"
        );

        let other_tree = ID::from_bytes("other_tree_id");
        let other_key = PublicKey::random();
        for (label, id) in [
            (
                "tree",
                request_id_for(&other_tree, &key, &Permission::Write(5)),
            ),
            (
                "key",
                request_id_for(&tree, &other_key, &Permission::Write(5)),
            ),
            (
                "priority",
                request_id_for(&tree, &key, &Permission::Write(4)),
            ),
            (
                "permission",
                request_id_for(&tree, &key, &Permission::Admin(5)),
            ),
            ("read", request_id_for(&tree, &key, &Permission::Read)),
        ] {
            assert_ne!(
                request_id_for(&tree, &key, &Permission::Write(5)),
                id,
                "a different {label} is a different request"
            );
        }
    }

    #[tokio::test]
    async fn test_get_nonexistent_request() {
        let (_instance, sync_tree, _clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let result = manager.get_request("nonexistent").await.unwrap();
        assert!(result.is_none());
    }
}
