//! Sync integration tests
//!
//! This module tests Sync functionality including creation, loading,
//! and settings management.

mod auto_sync_e2e_test;
mod auto_sync_tests;
mod basic_operations;
mod bidirectional_sync_test;
mod bootstrap_client_behavior_test;
mod bootstrap_concurrency_tests;
mod bootstrap_failure_tests;
mod bootstrap_policy_bug_test;
mod bootstrap_sync_tests;
mod bootstrap_with_key_tests;
mod chat_simulation_test;
mod dag_sync_tests;
mod device_id_tests;
pub mod helpers;
mod http_transport_tests;
mod integration_tests;
mod iroh_e2e_test;
mod iroh_transport_tests;
mod manual_approval_test;
mod peer_management_tests;
// mod queue_flush_tests; // Uses old queue API
mod sync_iroh_integration;
mod transport_conformance;
mod transport_integration_tests;
mod unified_message_handling_tests;
mod version_alignment_test;
