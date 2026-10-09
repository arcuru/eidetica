//! Private client assertions about an exact canonical source. These are never
//! used by daemon Store handlers, Entry verification or permission resolution.

use serde::{Deserialize, Serialize};

#[cfg(all(unix, feature = "service"))]
use crate::store::source::Reader;
use crate::{
    Result,
    backend::{
        BackendError, CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest,
    },
    store::source::StoreSource,
};

/// Physical representation identity, including Store-owned configuration bytes.
/// Unknown and encrypted inner formats are assertions, not daemon-validated code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateRepresentation {
    pub format: ProjectionDescriptor,
    pub configuration: Vec<u8>,
}

/// SDK opaque-cache framing, distinct from record-shaped representations.
pub(crate) const OPAQUE_ENCODING: &[u8] = b"eidetica/private-opaque-state/v0\0";

impl PrivateRepresentation {
    /// Identify the source-bound SDK envelope plus the Store's actual Codec.
    /// Canonical registration/configuration is additionally fixed by the source.
    pub fn opaque(format: ProjectionDescriptor, codec_identity: &str) -> Self {
        Self {
            format,
            configuration: [OPAQUE_ENCODING, codec_identity.as_bytes()].concat(),
        }
    }
}

#[cfg(all(unix, feature = "service"))]
pub(crate) const CHUNK_BYTES: usize = 1024 * 1024;

pub(crate) const PRIVATE_PROJECTION: &str = "eidetica/private-assistance";

/// Persisted in the backend's immutable token target, separate from value keys.
/// No per-daemon secret is needed to recover an acknowledged token. The socket
/// legacy paths cannot create or use this reserved target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Binding {
    pub source: StoreSource,
    pub principal: String,
    pub representation: PrivateRepresentation,
}

/// Published bytes have an immutable value identity; active/durable tokens
/// retain the full original Binding for per-use admission and exact recovery.
/// This changes no token or common-wire shape and applies only to this reserved
/// private Derived representation, never authoritative or legacy targets.
pub(crate) fn value_target(request: &StoreStateRequest) -> Result<StoreStateRequest> {
    let mut target = request.clone();
    if target.projection.name == PRIVATE_PROJECTION {
        let binding: Binding = serde_json::from_slice(&target.source_key)
            .map_err(|_| BackendError::InvalidStoreStateStagingToken)?;
        if target.lifecycle == StoreStateLifecycle::Authoritative
            || target.projection.version != 0
            || !matches!(target.scope, CacheScope::User(_))
            || binding.source.database != target.database
            || binding.source.store != target.store
            || !binding.source.seal.is_empty()
        {
            return Err(crate::backend::BackendError::InvalidStoreStateStagingToken.into());
        }
        target.source_key = serde_json::to_vec(&(
            "eidetica/private-value/v0",
            binding.principal,
            crate::store::query_records::binding(&binding.source, &binding.representation)?
                .as_bytes(),
        ))?;
    }
    Ok(target)
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
