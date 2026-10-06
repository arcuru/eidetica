//! Private client assertions about an exact canonical source. These are never
//! used by daemon Store handlers, Entry verification or permission resolution.

use serde::{Deserialize, Serialize};

use crate::backend::ProjectionDescriptor;
#[cfg(all(unix, feature = "service"))]
use crate::{
    Result,
    backend::{BackendError, CacheScope, StoreStateLifecycle, StoreStateRequest},
    store::source::{Reader, StoreSource},
};

/// Physical representation identity, including Store-owned configuration bytes.
/// Unknown and encrypted inner formats are assertions, not daemon-validated code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateRepresentation {
    pub format: ProjectionDescriptor,
    pub configuration: Vec<u8>,
}

#[cfg(all(unix, feature = "service"))]
pub(crate) const CHUNK_BYTES: usize = 1024 * 1024;

#[cfg(all(unix, feature = "service"))]
pub(crate) const PRIVATE_PROJECTION: &str = "eidetica/private-assistance";

/// Persisted in the backend's immutable token target and published source key.
/// No per-daemon secret is needed to recover an acknowledged token. The socket
/// legacy paths cannot create or use this reserved target.
#[cfg(all(unix, feature = "service"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Binding {
    pub source: StoreSource,
    pub principal: String,
    pub representation: PrivateRepresentation,
}
#[cfg(all(unix, feature = "service"))]
impl Binding {
    pub(crate) fn target(
        reader: &Reader,
        source: &StoreSource,
        representation: &PrivateRepresentation,
    ) -> Result<StoreStateRequest> {
        if representation.format.name.is_empty()
            || crate::store::source::encoded_size(representation, 16 * 1024).is_err()
        {
            return Err(BackendError::SourceTooLarge.into());
        }
        let mut source = source.clone();
        source.seal.clear();
        let binding = Self {
            source,
            principal: reader.principal.clone(),
            representation: representation.clone(),
        };
        Ok(StoreStateRequest {
            database: binding.source.database.clone(),
            store: binding.source.store.clone(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::User(reader.user.clone()),
            projection: ProjectionDescriptor {
                name: PRIVATE_PROJECTION.into(),
                version: 0,
            },
            source_key: serde_json::to_vec(&binding)?,
        })
    }

    pub(crate) fn from_target(reader: &Reader, target: &StoreStateRequest) -> Result<Self> {
        let invalid = || BackendError::InvalidStoreStateStagingToken;
        if target.lifecycle != StoreStateLifecycle::Derived
            || target.scope != CacheScope::User(reader.user.clone())
            || target.projection.name != PRIVATE_PROJECTION
            || target.projection.version != 0
        {
            return Err(invalid().into());
        }
        let binding: Self = serde_json::from_slice(&target.source_key).map_err(|_| invalid())?;
        if binding.principal != reader.principal
            || binding.source.database != target.database
            || binding.source.store != target.store
            || !binding.source.seal.is_empty()
        {
            return Err(invalid().into());
        }
        Ok(binding)
    }
}
