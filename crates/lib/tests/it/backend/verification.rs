use eidetica::{
    backend::{BackendImpl, VerificationStatus, database::InMemory},
    entry::{Entry, ID},
};
use std::sync::Arc;

use super::helpers::test_backend;
use crate::helpers::TestVerify;

#[tokio::test]
async fn test_verification_status_basic_operations() {
    let backend = test_backend().await;

    // Record initial counts (may be non-zero for backends with pre-seeded data)
    let initial_verified = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .unwrap()
        .len();
    let initial_failed = backend
        .get_entries_by_verification_status(VerificationStatus::Failed)
        .await
        .unwrap()
        .len();

    // Create a test entry
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry_id = entry.id();

    // Test storing with different verification statuses
    backend
        .put_verified(entry.clone())
        .await
        .expect("Failed to put verified entry");

    // Test getting verification status
    let status = backend
        .get_verification_status(&entry_id)
        .await
        .expect("Failed to get status");
    assert_eq!(status, VerificationStatus::Verified);

    // Test updating verification status
    backend
        .update_verification_status(&entry_id, VerificationStatus::Failed)
        .await
        .expect("Failed to update status");
    let updated_status = backend
        .get_verification_status(&entry_id)
        .await
        .expect("Failed to get updated status");
    assert_eq!(updated_status, VerificationStatus::Failed);

    // Test getting entries by verification status
    let failed_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Failed)
        .await
        .expect("Failed to get failed entries");
    assert_eq!(failed_entries.len(), initial_failed + 1);
    assert!(failed_entries.contains(&entry_id));

    let verified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .expect("Failed to get verified entries");
    // Our entry was moved to Failed, so verified count should be unchanged
    assert_eq!(verified_entries.len(), initial_verified);
}

#[tokio::test]
async fn test_verification_status_default_behavior() {
    let backend = test_backend().await;

    // Record initial verified count
    let initial_verified = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .unwrap()
        .len();

    // Create a test entry
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry_id = entry.id();

    // Store with Verified (default)
    backend
        .put_verified(entry)
        .await
        .expect("Failed to put entry");

    // Status should be Verified
    let status = backend
        .get_verification_status(&entry_id)
        .await
        .expect("Failed to get status");
    assert_eq!(status, VerificationStatus::Verified);

    // Should appear in verified entries
    let verified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .expect("Failed to get verified entries");
    assert_eq!(verified_entries.len(), initial_verified + 1);
    assert!(verified_entries.contains(&entry_id));
}

#[tokio::test]
async fn test_verification_status_multiple_entries() {
    let backend = test_backend().await;

    // Record initial counts
    let initial_verified = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .unwrap()
        .len();
    let initial_unverified = backend
        .get_entries_by_verification_status(VerificationStatus::Unverified)
        .await
        .unwrap()
        .len();

    // Create multiple test entries
    let entry1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry3 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    let entry1_id = entry1.id();
    let entry2_id = entry2.id();
    let entry3_id = entry3.id();

    // Store with different statuses
    backend
        .put_verified(entry1)
        .await
        .expect("Failed to put entry1");
    backend
        .put_verified(entry2)
        .await
        .expect("Failed to put entry2");
    backend.put(entry3).await.expect("Failed to put entry3");

    // Test filtering by status
    let verified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .expect("Failed to get verified entries");
    assert_eq!(verified_entries.len(), initial_verified + 2);
    assert!(verified_entries.contains(&entry1_id));
    assert!(verified_entries.contains(&entry2_id));

    // entry3 went in via plain `put` → Unverified (not Failed: it was
    // never checked-and-rejected, just not verified by this node).
    let unverified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Unverified)
        .await
        .expect("Failed to get unverified entries");
    assert_eq!(unverified_entries.len(), initial_unverified + 1);
    assert!(unverified_entries.contains(&entry3_id));
}

#[tokio::test]
async fn test_verification_status_not_found_errors() {
    let backend = test_backend().await;

    let nonexistent_id: ID = ID::from_bytes("nonexistent");

    // Test getting status for nonexistent entry
    let result = backend.get_verification_status(&nonexistent_id).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test updating status for nonexistent entry
    let mutable_backend = backend;
    let result = mutable_backend
        .update_verification_status(&nonexistent_id, VerificationStatus::Verified)
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[tokio::test]
#[cfg_attr(miri, ignore)] // file I/O not available with Miri isolation enabled
async fn test_verification_status_serialization() {
    let backend = Arc::new(InMemory::new());

    // Create test entries with different verification statuses
    let entry1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    let entry1_id = entry1.id();
    let entry2_id = entry2.id();

    backend
        .put_verified(entry1)
        .await
        .expect("Failed to put entry1");
    backend.put(entry2).await.expect("Failed to put entry2");

    // Save and load
    let temp_file = "/tmp/test_verification_status.json";
    backend
        .save_to_file(temp_file)
        .expect("Failed to save backend");

    let loaded_backend = InMemory::load_from_file(temp_file)
        .await
        .expect("Failed to load backend");

    // Verify statuses are preserved
    let status1 = loaded_backend
        .get_verification_status(&entry1_id)
        .await
        .expect("Failed to get status1");
    let status2 = loaded_backend
        .get_verification_status(&entry2_id)
        .await
        .expect("Failed to get status2");

    assert_eq!(status1, VerificationStatus::Verified);
    assert_eq!(status2, VerificationStatus::Unverified);

    // Clean up
    std::fs::remove_file(temp_file).ok();
}

#[tokio::test]
async fn test_backend_verification_helpers() {
    let backend = test_backend().await;

    // Record initial counts
    let initial_verified = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .unwrap()
        .len();
    let initial_unverified = backend
        .get_entries_by_verification_status(VerificationStatus::Unverified)
        .await
        .unwrap()
        .len();

    // Test the convenience methods
    let entry1 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry2 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let entry3 = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    let id1 = entry1.id();
    let id2 = entry2.id();
    let id3 = entry3.id();

    // Test the verified path (test helper: store + promote)
    backend
        .put_verified(entry1)
        .await
        .expect("Failed to put verified entry");
    assert_eq!(
        backend.get_verification_status(&id1).await.unwrap(),
        VerificationStatus::Verified
    );

    // Test the default store path: `put` always stores Unverified
    backend
        .put(entry2)
        .await
        .expect("Failed to put unverified entry");
    assert_eq!(
        backend.get_verification_status(&id2).await.unwrap(),
        VerificationStatus::Unverified
    );

    // Test explicit put method for comparison
    backend
        .put_verified(entry3)
        .await
        .expect("Failed to put with explicit status");
    assert_eq!(
        backend.get_verification_status(&id3).await.unwrap(),
        VerificationStatus::Verified
    );

    // Test that all entries are retrievable
    assert!(backend.get(&id1).await.is_ok());
    assert!(backend.get(&id2).await.is_ok());
    assert!(backend.get(&id3).await.is_ok());

    // Test get_entries_by_verification_status
    let verified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Verified)
        .await
        .unwrap();
    assert_eq!(verified_entries.len(), initial_verified + 2); // id1 and id3
    assert!(verified_entries.contains(&id1));
    assert!(verified_entries.contains(&id3));

    // id2 was stored via plain `put` → Unverified.
    let unverified_entries = backend
        .get_entries_by_verification_status(VerificationStatus::Unverified)
        .await
        .unwrap();
    assert_eq!(unverified_entries.len(), initial_unverified + 1); // id2
    assert!(unverified_entries.contains(&id2));
}

/// Regression: a re-`put` of an already-held entry must NOT touch its
/// verification status. Entries are content-addressed and immutable, and an
/// already-`Verified` entry is routinely re-received on overlapping/bootstrap
/// sync; demoting it back to `Unverified` there would silently cut the
/// Verified frontier and hide the entry (and its descendants) from default
/// reads until an unrelated write re-triggered verification.
#[tokio::test]
async fn test_reput_does_not_demote_existing_verified_entry() {
    let backend = Arc::new(InMemory::new());

    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let id = entry.id();

    // Local validation pass stores then promotes.
    backend
        .put_verified(entry.clone())
        .await
        .expect("Failed to put+promote entry");
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified
    );

    // Re-receiving the identical entry over the wire (sync overlap) must be a
    // no-op on status — it stays Verified, not demoted to Unverified.
    backend.put(entry.clone()).await.expect("Re-put failed");
    assert_eq!(
        backend.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Verified,
        "re-put of an already-Verified entry must not demote it"
    );

    // A re-`put` of an Unverified entry is likewise a no-op (stays
    // Unverified — not an error, not spuriously promoted).
    let u = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let uid: ID = u.id();
    backend.put(u.clone()).await.expect("Failed to put");
    backend.put(u).await.expect("Re-put failed");
    assert_eq!(
        backend.get_verification_status(&uid).await.unwrap(),
        VerificationStatus::Unverified
    );
}
