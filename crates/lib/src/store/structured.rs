use std::marker::PhantomData;

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use super::{
    Registered, Store, StoreStateModel,
    schema::{Attributed, StoreContract},
};
use crate::{Result, Transaction, crdt::CRDT};

/// Opt-in whole-state Store with exact per-contribution interpretation identity.
///
/// Uses structural Serde once at the whole-Store boundary. Existing Stores and
/// Entry bytes are unchanged. Mixed, unknown and untagged histories fail closed;
/// this API does not migrate data or coordinate schema rollouts.
///
/// ```no_run
/// use eidetica::{Database, Result, Store, crdt::Lww};
/// use eidetica::store::{StructuredStore, schema::{ContractId, StoreContract}};
/// struct Messages;
/// impl StoreContract for Messages {
///     const ID: ContractId = ContractId {
///         store: "structured:v0.1", data: "eidetica/lww:v0.1",
///         schema: "example/messages:v0.1",
///     };
/// }
/// # async fn example(db: Database) -> Result<()> {
/// let tx = db.new_transaction().await?;
/// let store = StructuredStore::<Lww<String>, Messages>::open(&tx, "message").await?;
/// store.apply(Lww::Set("hello".into())).await?;
/// assert!(store.read().await?.has_source());
/// tx.commit().await?;
/// # Ok(()) }
/// ```
pub struct StructuredStore<D, C: StoreContract> {
    transaction: Transaction,
    name: String,
    marker: PhantomData<fn() -> (D, C)>,
}

impl<D, C: StoreContract> Clone for StructuredStore<D, C> {
    fn clone(&self) -> Self {
        Self {
            transaction: self.transaction.clone(),
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<D, C: StoreContract> Registered for StructuredStore<D, C> {
    fn type_id() -> &'static str {
        C::ID.store
    }
}

#[async_trait]
impl<D, C> Store for StructuredStore<D, C>
where
    D: CRDT + Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static,
    C: StoreContract,
{
    type Data = Attributed<D, C>;

    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::opaque("eidetica/attributed-opaque", 1)
    }

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        txn.bind_structured::<C>(&name)
            .await
            .map_err(|e| txn.structured_error(&name, "load", e))?;
        Ok(Self {
            transaction: txn.clone(),
            name,
            marker: PhantomData,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
    fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    fn local_data(&self) -> Result<Option<Self::Data>> {
        use crate::crdt::Codec;
        self.transaction
            .with_structured_staging(&self.name, |staged| {
                Ok((staged.map(Attributed::decode).transpose()?, None))
            })
            .map_err(|e| {
                self.transaction
                    .structured_error(&self.name, "local data", e)
            })
    }
}

impl<D, C> StructuredStore<D, C>
where
    D: CRDT + Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static,
    C: StoreContract,
{
    async fn historical(&self) -> Result<Attributed<D, C>> {
        self.transaction
            .get_full_state_with_descriptor(
                &self.name,
                self.transaction
                    .encrypted_projection_descriptor(&self.name, Self::state_model().descriptor()),
                Some(Attributed::has_source),
            )
            .await
    }

    /// Read captured historical state plus the atomically captured staged delta.
    /// Empty history has no recorded descriptor; `into_value` returns D::default.
    pub async fn read(&self) -> Result<Attributed<D, C>> {
        let result = async {
            let history = self.historical().await?;
            self.transaction
                .with_structured_staging(&self.name, |staged| {
                    use crate::crdt::Codec;
                    let value = match staged {
                        Some(bytes) => history.merge(&Attributed::<D, C>::decode(bytes)?)?,
                        None => history,
                    };
                    Ok((value, None))
                })
        }
        .await;
        result.map_err(|e| self.transaction.structured_error(&self.name, "read", e))
    }

    /// Compose, preview and encode before replacing staging under one lock.
    /// Every failure preserves earlier staged bytes, even if the caller commits.
    pub async fn apply(&self, delta: D) -> Result<()> {
        let result = async {
            let history = self.historical().await?;
            self.transaction
                .with_structured_staging(&self.name, |staged| {
                    use crate::crdt::Codec;
                    let delta = Attributed::<D, C>::new(delta);
                    let composed = match staged {
                        Some(bytes) => Attributed::<D, C>::decode(bytes)?.merge(&delta)?,
                        None => delta,
                    };
                    let preview = history.merge(&composed)?;
                    // Qualify both the durable delta and resulting typed view. The
                    // round-trip rejects a custom serializer/decoder that loses state.
                    let bytes = composed.encode()?;
                    Attributed::<D, C>::decode(&bytes)?;
                    Attributed::<D, C>::decode(&preview.encode()?)?;
                    Ok(((), Some(bytes)))
                })
        }
        .await;
        result.map_err(|e| self.transaction.structured_error(&self.name, "apply", e))
    }
}
