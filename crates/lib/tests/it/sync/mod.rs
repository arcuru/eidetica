//! Sync integration tests
//!
//! This module tests Sync functionality including creation, loading,
//! and settings management.

// Disabled due to old API references - needs updating for new BackgroundSync architecture
// mod background_sync_tests;
mod basic_operations;
mod bootstrap_sync_tests;
mod chat_simulation_test;
mod dag_sync_tests;
mod device_id_tests;
// mod end_to_end_sync_tests; // Uses old sync_queue API
pub mod helpers;
mod http_transport_tests;
mod integration_tests;
mod iroh_e2e_test;
mod iroh_transport_tests;
mod peer_management_tests;
// mod queue_flush_tests; // Uses old queue API
mod sync_iroh_integration;
// mod sync_protocol_tests; // Uses old protocol API
mod transport_conformance;
mod transport_integration_tests;
mod unified_message_handling_tests;
mod version_alignment_test;
