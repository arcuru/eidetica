use std::marker::PhantomData;

use async_trait::async_trait;

use super::{
    Registered, Store, StoreError, StoreStateModel,
    password_store::PASSWORD_TYPE,
    schema::{StructuredState, StructuredType},
};
use crate::{Result, Transaction, crdt::CRDT};

/// Opt-in whole-state Store whose contributions keep the Type they were written
/// under.
///
/// Each contribution is the Type's native canonical DAG-CBOR value. Its Type is
/// the one declared in `_index` at the contributing Entry, including a
/// declaration in that Entry. A commit emits bytes of the declared Type or fails.
/// Reads refuse any history that is not entirely of this exact Type.
///
/// ```no_run
/// use eidetica::{Database, Result, Store, crdt::{Doc, Lww}};
/// use eidetica::store::{StructuredStore, schema::StructuredType};
/// struct Messages;
/// impl StructuredType for Messages {
///     const TYPE_ID: &'static str = "example/messages:v0.1";
///     type Data = Lww<String>;
///     fn grammar(_: &[Doc]) -> Result<Doc> {
///         Ok(Doc::new())
///     }
/// }
/// # async fn example(db: Database) -> Result<()> {
/// let tx = db.new_transaction().await?;
/// let store = StructuredStore::<Messages>::open(&tx, "message").await?;
/// store.apply(Lww::Set("hello".into())).await?;
/// assert!(store.read().await?.has_source());
/// tx.commit().await?;
/// # Ok(()) }
/// ```
pub struct StructuredStore<T: StructuredType> {
    transaction: Transaction,
    name: String,
    marker: PhantomData<fn() -> T>,
}

impl<T: StructuredType> Clone for StructuredStore<T> {
    fn clone(&self) -> Self {
        Self {
            transaction: self.transaction.clone(),
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: StructuredType> Registered for StructuredStore<T> {
    fn type_id() -> &'static str {
        T::TYPE_ID
    }
}

#[async_trait]
impl<T: StructuredType> Store for StructuredStore<T> {
    type Data = StructuredState<T>;

    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::opaque("eidetica/structured-opaque", 3)
    }

    fn uses_literal_registration() -> bool {
        true
    }

    async fn register(txn: &Transaction, name: String) -> Result<Self> {
        Self::stage_declaration(txn, name, Self::default_config()).await
    }

    /// Patches config only. Does not declare a Type.
    async fn set_config(&self, config: crate::crdt::Doc) -> Result<()> {
        if self.transaction.structured_is_encrypted(&self.name) {
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "set_config".into(),
                reason: "wrapped configuration must be changed through PasswordStore".into(),
            }
            .into());
        }
        self.transaction
            .get_index()
            .await?
            .set_literal_config(&self.name, config)
            .await?;
        self.transaction.stage_config_writer(&self.name, T::TYPE_ID)
    }

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        if T::TYPE_ID.is_empty() || name.is_empty() || name.starts_with('_') {
            return Err(StoreError::InvalidOperation {
                store: name,
                operation: "load".into(),
                reason: "invalid structured Store name or Type".into(),
            }
            .into());
        }
        txn.mark_literal_registration(&name)?;
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
        self.transaction
            .with_structured_staging::<T, _>(&self.name, |staged, _| Ok((staged, None)))
            .map_err(|e| self.redact("local data", e))
    }
}

impl<T: StructuredType> StructuredStore<T> {
    fn redact(&self, operation: &str, error: crate::Error) -> crate::Error {
        self.transaction
            .structured_error(&self.name, operation, error)
    }

    async fn stage_declaration(
        txn: &Transaction,
        name: String,
        config: crate::crdt::Doc,
    ) -> Result<Self> {
        let store = Self::load(txn, name).await?;
        let index = txn.get_index().await?;
        index.check_registration_alias(&store.name, true).await?;
        index
            .set_literal_entry(&store.name, T::TYPE_ID, config)
            .await?;
        Ok(store)
    }

    /// Declare this Type for `name` from the current Entry on, with its complete
    /// config, and open the Store.
    ///
    /// This is the explicit choice that changes a Type or settles conflicting
    /// declarations. It neither adds a contribution nor reinterprets, converts or
    /// repairs earlier ones. Values staged under another Type make the commit fail.
    pub async fn declare(
        txn: &Transaction,
        name: impl Into<String> + Send,
        config: crate::crdt::Doc,
    ) -> Result<Self> {
        let name = name.into();
        // A plaintext declaration must not silently end a Store's encryption.
        if txn.structured_is_encrypted(&name)
            || matches!(txn.get_index().await?.entry(&name, true).await,
                Ok(row) if row.type_id == PASSWORD_TYPE)
        {
            return Err(StoreError::InvalidOperation {
                store: name,
                operation: "declare".into(),
                reason: "change a wrapped Type through the unlocked PasswordStore".into(),
            }
            .into());
        }
        // The whole config, not a patch over the previous Type's config.
        let complete = crate::crdt::Doc::atomic().merge(&config)?;
        Self::stage_declaration(txn, name, complete).await
    }

    /// Read captured history plus the staged delta.
    pub async fn read(&self) -> Result<StructuredState<T>> {
        async {
            let history = self.transaction.read_structured::<T>(&self.name).await?;
            self.transaction
                .with_structured_staging::<T, _>(&self.name, |staged, _| {
                    let state = match staged {
                        Some(staged) => history.state.merge(&staged)?,
                        None => history.state,
                    };
                    Ok((state, None))
                })
        }
        .await
        .map_err(|e| self.redact("read", e))
    }

    /// Compose, preview and encode before replacing staging under one lock.
    /// Every failure preserves earlier staged bytes.
    pub async fn apply(&self, delta: T::Data) -> Result<()> {
        async {
            let history = self.transaction.read_structured::<T>(&self.name).await?;
            let writer_grammar = history.writer_grammar?;
            self.transaction
                .with_structured_staging::<T, _>(&self.name, |staged, grammar| {
                    use crate::crdt::Codec;
                    if [history.grammar.as_ref(), grammar]
                        .into_iter()
                        .flatten()
                        .any(|g| g != &writer_grammar)
                    {
                        return Err(super::schema::invalid(
                            "Store config does not match the values being extended",
                        ));
                    }
                    let delta = StructuredState::<T>::new(delta);
                    let composed = match staged {
                        Some(staged) => staged.merge(&delta)?,
                        None => delta,
                    };
                    // Qualify both the durable delta and the resulting view. The
                    // round trip rejects a serializer that loses state.
                    let bytes = composed.encode()?;
                    StructuredState::<T>::decode(&bytes)?;
                    StructuredState::<T>::decode(&history.state.merge(&composed)?.encode()?)?;
                    Ok(((), Some((bytes, writer_grammar))))
                })
        }
        .await
        .map_err(|e| self.redact("apply", e))
    }
}
