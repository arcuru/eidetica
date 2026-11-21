//! Admission for client-built Derived state, independent of database roles.

use super::{
    BackendError, DerivedCachePolicy, RecordMutations, StoreStateLifecycle, StoreStateRequest,
};

pub(super) const RETAINED: DerivedCachePolicy = DerivedCachePolicy {
    max_namespaces: 128,
    max_bytes: 64 * 1024 * 1024,
};
pub(super) const STAGING_PER_LOGIN: DerivedCachePolicy = DerivedCachePolicy {
    max_namespaces: 8,
    max_bytes: 64 * 1024 * 1024,
};
pub(super) const STAGING_TOTAL: DerivedCachePolicy = DerivedCachePolicy {
    max_namespaces: 64,
    max_bytes: 256 * 1024 * 1024,
};

// Proof-only sessions have generated, non-reusable cache scopes. They share
// admission, not visibility, so a fresh connection cannot mint a new budget.
pub(crate) const SIGNER_SCOPE_PREFIX: &str = "signer:";

pub(super) fn bucket(scope: &str) -> &str {
    if scope.starts_with(SIGNER_SCOPE_PREFIX) {
        SIGNER_SCOPE_PREFIX
    } else {
        scope
    }
}

pub(super) fn login(request: &StoreStateRequest) -> Option<&str> {
    (request.lifecycle == StoreStateLifecycle::Derived)
        .then(|| request.scope.storage_key())
        .flatten()
        .filter(|id| !id.is_empty())
        .map(bucket)
}

pub(super) fn check_target(request: &StoreStateRequest) -> crate::Result<()> {
    // Record budgets do not account for the coordinates retained with each build.
    if request.store.len() + request.projection.name.len() + request.source_key.len() > 16 * 1024 {
        return Err(refused("private cache target too large").into());
    }
    Ok(())
}

pub(super) fn chunk_charge(records: &RecordMutations) -> u64 {
    // Charge cumulatively until the build is gone; replacement and delete
    // markers cannot hide transient work or an unbounded number of tiny rows.
    records
        .iter()
        .map(|(key, value)| {
            32 + key.len() as u64 + value.as_ref().map_or(0, |value| value.len() as u64)
        })
        .sum()
}

pub(super) fn check(
    policy: DerivedCachePolicy,
    count: usize,
    bytes: u64,
    reason: &str,
) -> crate::Result<()> {
    if count > policy.max_namespaces || bytes > policy.max_bytes {
        return Err(refused(reason).into());
    }
    Ok(())
}

fn refused(reason: &str) -> BackendError {
    BackendError::StoreStateNotRetained {
        reason: reason.into(),
    }
}
