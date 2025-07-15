//! Tree integration tests
//!
//! This module contains tests for Tree functionality including core operations,
//! API methods, parent-aware merging, and settings metadata management.
//! Tests are organized by functional area for better maintainability.
//!
//! ## Test Organization
//!
//! - `core_operations`: Basic tree operations, entry management, tips handling
//! - `api_methods`: Tree API methods for entry retrieval, authentication, validation
//! - `merge_algorithms`: Parent-aware merging, LCA computation, complex DAG scenarios
//! - `settings_metadata`: Settings tracking, metadata management, tips propagation
//! - `helpers`: Comprehensive helper functions for tree testing

mod api_methods;
mod core_operations;
mod helpers;
mod merge_algorithms;
mod settings_metadata;
