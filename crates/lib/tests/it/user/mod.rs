//! User module integration tests

// Allow deprecated methods in tests - some tests use old sync API for backwards compatibility
#![allow(deprecated)]

mod database_operations_tests;
mod database_tracking_tests;
mod helpers;
mod integration_tests;
mod key_management_tests;
mod multi_user_tests;
mod security_tests;
mod user_bootstrap_tests;
mod user_lifecycle_tests;
