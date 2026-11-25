//! Historical attribution is evaluated in the signed main DAG. Store links
//! determine replay order and cache boundaries, never the declaration frontier.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
};

use serde::{Serialize, de::DeserializeOwned};

use super::{Transaction, TransactionError};
use crate::{
    Entry, Error, Result, Snapshot,
    backend::{CacheScope, StoreStateRequest, VerificationStatus},
    constants::INDEX,
    crdt::{CRDT, Codec, Doc, doc::Value},
    entry::{EntryBuilder, ID},
    store::{
        ProjectionDescriptor, StoreError,
        attribution::{self, Declaration},
        schema::{self, Attributed, ContractDescriptor, ContributionAttribution, StoreContract},
        state,
    },
};

use schema::invalid;

struct MainHistory {
    entries: Vec<Entry>,
    by_id: HashMap<ID, usize>,
    parents: Vec<Vec<usize>>,
}

impl MainHistory {
    async fn load(tx: &Transaction, tips: &[ID]) -> Result<Self> {
        let mut entries: HashMap<ID, Entry> = HashMap::new();
        let backend = tx.db.ops();
        let local = backend.local_engine();
        if local.is_none() && !tips.is_empty() {
            #[cfg(not(all(unix, feature = "service")))]
            return Err(invalid("verified attribution history unavailable"));
            #[cfg(all(unix, feature = "service"))]
            {
                if backend.remote_connection().is_none() {
                    return Err(invalid("verified attribution history unavailable"));
                }
                // Raw service GetEntry is not a verification verdict. Prove the
                // captured tips are reachable from the service's verified
                // frontier, without using that frontier as the replay boundary.
                let mut pending = backend.snapshot(tx.db.root_id()).await?.into_tips();
                let mut required: HashSet<_> = tips.iter().cloned().collect();
                while !required.is_empty() {
                    let id = pending
                        .pop()
                        .ok_or_else(|| invalid("captured attribution tips are not verified"))?;
                    if !entries.contains_key(&id) {
                        let entry = checked_entry(tx, &id).await?;
                        pending.extend(entry.parents()?);
                        entries.insert(id.clone(), entry);
                    }
                    required.remove(&id);
                }
            }
        }

        // Iterative postorder: topological order, without depending on a
        // timestamp/height policy or recursion depth. Every dependency is
        // fetched through this handle's same authorized read seam.
        let mut history = Self {
            entries: Vec::new(),
            by_id: HashMap::new(),
            parents: Vec::new(),
        };
        let mut pending: Vec<_> = tips.iter().cloned().map(|id| (id, false)).collect();
        let mut visiting = HashSet::new();
        while let Some((id, ready)) = pending.pop() {
            if history.by_id.contains_key(&id) {
                continue;
            }
            if ready {
                let entry = entries
                    .remove(&id)
                    .ok_or_else(|| invalid("incomplete main history"))?;
                let parents = entry
                    .parents()?
                    .iter()
                    .map(|p| history.position(p))
                    .collect::<Result<_>>()?;
                history.parents.push(parents);
                history.by_id.insert(id.clone(), history.entries.len());
                history.entries.push(entry);
                visiting.remove(&id);
                continue;
            }
            if !visiting.insert(id.clone()) {
                return Err(invalid("cyclic main history"));
            }
            if let Some(engine) = &local
                && engine.get_verification_status(&id).await? != VerificationStatus::Verified
            {
                return Err(invalid("attribution history is not verified"));
            }
            let entry = match entries.get(&id) {
                Some(entry) => entry,
                None => {
                    let entry = checked_entry(tx, &id).await?;
                    entries.entry(id.clone()).or_insert(entry)
                }
            };
            let parents = entry.parents()?;
            if parents.is_empty() != (&id == tx.db.root_id()) {
                return Err(invalid("main attribution history does not end at its root"));
            }
            pending.push((id, true));
            pending.extend(parents.into_iter().map(|parent| (parent, false)));
        }
        Ok(history)
    }

    fn position(&self, id: &ID) -> Result<usize> {
        self.by_id
            .get(id)
            .copied()
            .ok_or_else(|| invalid("Store link is outside the verified main history"))
    }

    fn store_order(&self, store: &str, tips: &[ID]) -> Result<Vec<usize>> {
        let mut seen = HashSet::new();
        let mut pending = tips.to_vec();
        while let Some(id) = pending.pop() {
            let n = self.position(&id)?;
            if seen.insert(n) {
                pending.extend(self.entries[n].subtree_parents(store)?);
            }
        }
        let mut order = seen
            .into_iter()
            .map(|n| {
                Ok((
                    self.entries[n].subtree_height(store)?,
                    self.entries[n].id_ref(),
                    n,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        order.sort_unstable();
        Ok(order.into_iter().map(|(_, _, n)| n).collect())
    }

    fn store_tips(&self, store: &str) -> Result<Snapshot> {
        let mut tips = BTreeSet::new();
        for entry in &self.entries {
            if entry.in_subtree(store) {
                tips.insert(entry.id());
                for p in entry.subtree_parents(store)? {
                    // Never treat an omitted/missing dependency as a branch.
                    if !self.entries[self.position(&p)?].in_subtree(store) {
                        return Err(invalid("parent does not contribute to the Store history"));
                    }
                    tips.remove(&p);
                }
            }
        }
        Ok(Snapshot::from(tips.into_iter().collect::<Vec<_>>()))
    }
}

async fn checked_entry(tx: &Transaction, id: &ID) -> Result<Entry> {
    let entry = tx.db.ops().get(id).await?;
    if entry.id_ref() != id || !entry.in_tree(tx.db.root_id()) {
        return Err(invalid("wrong source Entry for attribution history"));
    }
    Ok(entry)
}

struct HistoricalDefaults<'a> {
    history: &'a MainHistory,
    declarations: HashMap<usize, Declaration>,
    frontiers: Vec<BTreeSet<usize>>,
    reachability: HashMap<(usize, usize), bool>,
    decoded: HashMap<usize, ContractDescriptor>,
}

impl<'a> HistoricalDefaults<'a> {
    fn new(history: &'a MainHistory, store: &str) -> Self {
        let mut defaults = Self {
            history,
            declarations: HashMap::new(),
            frontiers: Vec::with_capacity(history.entries.len()),
            reachability: HashMap::new(),
            decoded: HashMap::new(),
        };
        for (n, entry) in history.entries.iter().enumerate() {
            let mut frontier = defaults.inherited(&history.parents[n]);
            if let Some(declaration) = entry
                .data(INDEX)
                .ok()
                .and_then(|bytes| attribution::project(bytes, store))
                && !defaults.leaves_declaration(&frontier, &declaration)
            {
                defaults.declarations.insert(n, declaration);
                frontier = BTreeSet::from([n]);
            }
            defaults.frontiers.push(frontier);
        }
        defaults
    }

    fn inherited(&mut self, parents: &[usize]) -> BTreeSet<usize> {
        if let [parent] = parents {
            return self.frontiers[*parent].clone();
        }
        let mut maximal = BTreeSet::new();
        let candidates: BTreeSet<_> = parents
            .iter()
            .flat_map(|p| self.frontiers[*p].iter().copied())
            .collect();
        // Parent sets were already reduced. Compare origins only at joins;
        // the topological index is a pruning bound, never a winner rule.
        for origin in candidates.into_iter().rev() {
            if !maximal.iter().any(|&later| self.is_ancestor(origin, later)) {
                maximal.insert(origin);
            }
        }
        maximal
    }

    fn is_ancestor(&mut self, earlier: usize, later: usize) -> bool {
        let parents = &self.history.parents;
        *self
            .reachability
            .entry((earlier, later))
            .or_insert_with(|| {
                let mut pending = vec![later];
                let mut seen = HashSet::new();
                while let Some(n) = pending.pop() {
                    if n == earlier {
                        return true;
                    }
                    if n > earlier && seen.insert(n) {
                        pending.extend(&parents[n]);
                    }
                }
                false
            })
    }

    fn leaves_declaration(&self, frontier: &BTreeSet<usize>, declaration: &Declaration) -> bool {
        match declaration {
            Declaration::Wrapped(_) => frontier
                .iter()
                .any(|origin| &self.declarations[origin] == declaration),
            Declaration::PlainConfigPatch => {
                !frontier.is_empty()
                    && frontier
                        .iter()
                        .all(|origin| matches!(self.declarations[origin], Declaration::Plain(_)))
            }
            _ => false,
        }
    }

    fn resolve(
        &mut self,
        tx: &Transaction,
        store: &str,
        frontier: &BTreeSet<usize>,
    ) -> Result<ContractDescriptor> {
        let mut resolved = None;
        for &origin in frontier {
            let id = if let Some(id) = self.decoded.get(&origin) {
                id.clone()
            } else {
                let id = tx.resolve_declaration(store, &self.declarations[&origin])?;
                self.decoded.insert(origin, id.clone());
                id
            };
            if resolved.as_ref().is_some_and(|previous| previous != &id) {
                return Err(invalid(
                    "incomparable historical Store declarations disagree",
                ));
            }
            resolved = Some(id);
        }
        resolved.ok_or_else(|| invalid("no historical Store declaration"))
    }
}

impl Transaction {
    pub(super) fn freeze_structured(&mut self) -> Result<bool> {
        let (builder, literals, contracts, encryptors, record_deltas) = {
            let mut builder = self.entry_builder.lock().unwrap();
            let literals = self.literal_registrations.lock().unwrap();
            if literals.is_empty() {
                return Ok(false); // Preserve the legacy transaction path.
            }
            let builder = builder
                .take()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            (
                builder,
                literals.clone(),
                self.structured_contracts.lock().unwrap().clone(),
                self.encryptors.lock().unwrap().clone(),
                self.logical_record_mutations.lock().unwrap().clone(),
            )
        };
        self.entry_builder = Arc::new(Mutex::new(Some(builder)));
        self.literal_registrations = Arc::new(Mutex::new(literals));
        self.structured_contracts = Arc::new(Mutex::new(contracts));
        self.encryptors = Arc::new(Mutex::new(encryptors));
        self.logical_record_mutations = Arc::new(Mutex::new(record_deltas));
        Ok(true)
    }

    pub(crate) fn mark_literal_registration(&self, name: &str) {
        self.literal_registrations
            .lock()
            .unwrap()
            .insert(name.into());
    }

    pub(crate) fn structured_is_encrypted(&self, store: &str) -> bool {
        self.encryptors.lock().unwrap().contains_key(store)
    }

    pub(crate) fn structured_error(&self, store: &str, operation: &str, error: Error) -> Error {
        // Application serializers and mergers can return arbitrary errors; no
        // error text from an encrypted structured operation is safe to echo.
        if self.structured_is_encrypted(store) {
            StoreError::InvalidOperation {
                store: store.into(),
                operation: operation.into(),
                reason: "encrypted structured operation failed".into(),
            }
            .into()
        } else {
            error
        }
    }

    pub(crate) async fn bind_structured<C: StoreContract>(&self, store: &str) -> Result<()> {
        let expected = ContractDescriptor::from(C::ID);
        attribution::validate_structured_store(store, &expected)?;
        let unlocked = self
            .encryptors
            .lock()
            .unwrap()
            .get(store)
            .map(|e| e.supports_store_type(C::ID.store));
        match unlocked {
            Some(false) => return Err(invalid("wrapped Store type mismatch")),
            Some(true) => {
                schema::protect(Some(&expected))?;
            }
            None => match self.get_index().await?.entry(store, true).await {
                Ok(info) if info.type_id != C::ID.store => {
                    return Err(StoreError::TypeMismatch {
                        store: store.into(),
                        expected: C::ID.store.into(),
                        actual: info.type_id,
                    }
                    .into());
                }
                Ok(_) => {}
                Err(Error::Store(e)) if matches!(e.as_ref(), StoreError::KeyNotFound { store: s, key } if s == INDEX && key == store) =>
                {
                    // A pre-existing path-based legacy registration may not be
                    // replaced by adding a literal row for the same subtree.
                    self.get_index()
                        .await?
                        .check_registration_alias(store, true)
                        .await?;
                }
                Err(e) => return Err(e),
            },
        }
        let builder = self.entry_builder.lock().unwrap();
        if builder.is_none() {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        self.mark_literal_registration(store);
        let mut bindings = self.structured_contracts.lock().unwrap();
        if bindings.get(store).is_some_and(|bound| bound != &expected) {
            return Err(invalid(
                "structured Store already bound to another contract",
            ));
        }
        bindings.insert(store.into(), expected);
        Ok(())
    }

    /// Serialize decode/compose/preview/replace across cloned handles.
    pub(crate) fn with_structured_staging<T>(
        &self,
        store: &str,
        apply: impl FnOnce(Option<&[u8]>) -> Result<(T, Option<Vec<u8>>)>,
    ) -> Result<T> {
        let mut guard = self.entry_builder.lock().unwrap();
        let builder = guard
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        let (result, bytes) = apply(builder.data(store).ok().map(Vec::as_slice))?;
        if let Some(bytes) = bytes {
            builder.set_subtree_data_mut(store.to_string(), bytes);
        }
        Ok(result)
    }

    pub(crate) async fn stage_registry_patch(
        &self,
        index: &str,
        store: &str,
        patch: Doc,
    ) -> Result<()> {
        self.init_subtree_parents(index).await?;
        self.mark_literal_registration(store);
        self.with_structured_staging(index, |bytes| {
            let mut index = bytes.map(Doc::decode).transpose()?.unwrap_or_default();
            let row = match index.get_literal(store) {
                Some(Value::Doc(row)) => row.merge(&patch)?,
                _ => patch,
            };
            index.set_literal(store, Value::Doc(row));
            Ok(((), Some(index.encode()?)))
        })
    }

    fn resolve_declaration(
        &self,
        store: &str,
        declaration: &Declaration,
    ) -> Result<ContractDescriptor> {
        let encryptors = self.encryptors.lock().unwrap();
        let encryptor = encryptors.get(store);
        match declaration {
            Declaration::Plain(id) if encryptor.is_none() => Ok(id.clone()),
            Declaration::Wrapped(config) if encryptor.is_some() => {
                encryptors[store].historical_descriptor(config)
            }
            Declaration::Wrapped(_) => Err(invalid("historical wrapped configuration is locked")),
            Declaration::Plain(_) => Err(invalid("historical Store is not encrypted")),
            Declaration::Legacy => Err(invalid("historical Store is legacy and unattributed")),
            Declaration::Missing => Err(invalid(
                "historical Store declaration was removed or never initialized",
            )),
            Declaration::Malformed | Declaration::PlainConfigPatch => {
                Err(invalid("malformed historical Store declaration"))
            }
            Declaration::Unsupported => Err(invalid("unsupported historical Store registration")),
        }
    }

    pub(super) async fn finalize_structured(&self, builder: &mut EntryBuilder) -> Result<()> {
        let bindings: Vec<_> = self
            .structured_contracts
            .lock()
            .unwrap()
            .iter()
            .filter(|(store, _)| builder.data(store).is_ok())
            .map(|(store, id)| (store.clone(), id.clone()))
            .collect();
        if bindings.is_empty() && builder.data(INDEX).is_err() {
            return Ok(());
        }
        let parents = builder.parents()?;
        let history = MainHistory::load(self, &parents).await?;
        let positions = parents
            .iter()
            .map(|id| history.position(id))
            .collect::<Result<Vec<_>>>()?;
        let staged_index = builder.data(INDEX).ok().cloned();
        for (store, bound) in bindings {
            let finalized = (|| {
                let mut defaults = HistoricalDefaults::new(&history, &store);
                let inherited = defaults.inherited(&positions);
                let default = match staged_index
                    .as_deref()
                    .and_then(|bytes| attribution::project(bytes, &store))
                {
                    Some(declaration) if !defaults.leaves_declaration(&inherited, &declaration) => {
                        self.resolve_declaration(&store, &declaration)?
                    }
                    _ => defaults.resolve(self, &store, &inherited)?,
                };
                schema::finalize_contribution(
                    builder.data(&store)?,
                    &bound,
                    &default,
                    self.structured_is_encrypted(&store),
                )
            })()
            .map_err(|e| self.structured_error(&store, "commit", e))?;
            builder.set_subtree_data_mut(store, finalized);
        }
        Ok(())
    }

    async fn structured_history(&self, store: &str) -> Result<(MainHistory, Snapshot, Vec<usize>)> {
        self.init_subtree_parents(store).await?;
        let (main, subtree) = {
            let guard = self.entry_builder.lock().unwrap();
            let builder = guard
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            (
                builder.parents()?,
                Snapshot::from(builder.subtree_parents(store)?),
            )
        };
        let history = MainHistory::load(self, &main).await?;
        let actual = history.store_tips(store)?;
        if actual != subtree {
            return Err(invalid(
                "captured Store tips do not cover the verified main history",
            ));
        }
        let order = history.store_order(store, actual.tips())?;
        Ok((history, actual, order))
    }

    fn resolve_contribution(
        &self,
        store: &str,
        store_type: &str,
        defaults: &mut HistoricalDefaults<'_>,
        n: usize,
    ) -> Result<(ContributionAttribution, ipld_core::ipld::Ipld)> {
        let frontier = defaults.frontiers[n].clone();
        let default = defaults.resolve(self, store, &frontier)?;
        if default.store != store_type {
            return Err(invalid("unsupported historical Store grammar"));
        }
        let entry = &defaults.history.entries[n];
        let bytes = self.decrypt_if_needed(store, entry.data(store)?)?;
        let (override_id, value) =
            schema::contribution(&bytes, &default, self.structured_is_encrypted(store))?;
        Ok((
            ContributionAttribution {
                database: self.db.root_id().clone(),
                store: store.into(),
                entry: entry.id(),
                declaration_origins: frontier
                    .into_iter()
                    .map(|n| defaults.history.entries[n].id())
                    .collect(),
                default,
                override_id,
            },
            value,
        ))
    }

    pub(crate) async fn inspect_structured(
        &self,
        store: &str,
        store_type: &str,
        id: &ID,
    ) -> Result<Option<ContributionAttribution>> {
        let (history, _, order) = self.structured_history(store).await?;
        let n = history.position(id)?;
        if !order.contains(&n) || history.entries[n].data(store).is_err() {
            return Ok(None);
        }
        let mut defaults = HistoricalDefaults::new(&history, store);
        let (identity, _) = self.resolve_contribution(store, store_type, &mut defaults, n)?;
        Ok(Some(identity))
    }

    pub(crate) async fn read_structured<D, C>(
        &self,
        store: &str,
        descriptor: ProjectionDescriptor,
    ) -> Result<Attributed<D, C>>
    where
        D: CRDT + Clone + Serialize + DeserializeOwned + Send,
        C: StoreContract,
    {
        let (history, boundary, order) = self.structured_history(store).await?;
        let mut defaults = HistoricalDefaults::new(&history, store);
        let expected = ContractDescriptor::from(C::ID);
        let mut values = Vec::new();
        // Gate the complete source before warm cache decode, application
        // deserialization or merge. The cache does not prove source history.
        for &n in &order {
            if history.entries[n].data(store).is_ok() {
                let (id, value) =
                    self.resolve_contribution(store, C::ID.store, &mut defaults, n)?;
                if id.effective() != &expected {
                    return Err(invalid("unsupported Store/data/application contract"));
                }
                values.push((n, value));
            }
        }
        if values.is_empty() {
            return Ok(Attributed::default());
        }
        let request = |boundary: &Snapshot| {
            state::opaque_request(
                self.db.root_id(),
                store,
                descriptor.clone(),
                boundary.cache_key_bytes(),
                CacheScope::Shared,
            )
        };
        let target = request(&boundary);
        if let Some(bytes) = state::load_cached(self.db.ops(), &target).await? {
            return self.decode_structured_cache(store, &bytes);
        }
        let mut result = Attributed::default();
        let mut skip = 0;
        if boundary.len() > 1 {
            let merge = self
                .db
                .ops()
                .compute_merge_state(self.db.root_id(), store, boundary.tips())
                .await?;
            if let Some(base) = merge.merge_base {
                let prefix = history.store_order(store, std::slice::from_ref(&base))?;
                // A cached prefix may only replace precisely that prefix of
                // canonical fold order (not a late interleaved branch).
                if order.starts_with(&prefix)
                    && values.first().is_some_and(|(n, _)| prefix.contains(n))
                    && let Some(bytes) =
                        state::load_cached(self.db.ops(), &request(&Snapshot::from([base]))).await?
                {
                    result = self.decode_structured_cache(store, &bytes)?;
                    let prefix: HashSet<_> = prefix.into_iter().collect();
                    skip = values
                        .iter()
                        .take_while(|(n, _)| prefix.contains(n))
                        .count();
                }
            }
        }
        for (_, value) in values.into_iter().skip(skip) {
            result = result.merge(&Attributed::decode_value(&value)?)?;
        }
        self.publish_structured_cache(store, target, result).await
    }

    fn decode_structured_cache<D: Serialize + DeserializeOwned, C: StoreContract>(
        &self,
        store: &str,
        bytes: &[u8],
    ) -> Result<Attributed<D, C>> {
        Attributed::decode_cached(
            &self.decrypt_if_needed(store, bytes)?,
            self.structured_is_encrypted(store),
        )
    }

    pub(super) async fn publish_structured_cache<
        D: Serialize + DeserializeOwned + Send,
        C: StoreContract,
    >(
        &self,
        store: &str,
        request: StoreStateRequest,
        value: Attributed<D, C>,
    ) -> Result<Attributed<D, C>> {
        let encrypted = self.structured_is_encrypted(store);
        let (value, view) = state::store_cached(self.db.ops(), request, value, |result| {
            self.encrypt_if_needed(store, &result.encode_cached(encrypted)?)
        })
        .await?;
        if let Some(view) = view {
            match state::load_opaque(self.db.ops(), &view).await {
                Ok(Some(bytes)) => {
                    self.decode_structured_cache::<D, C>(store, &bytes)?;
                }
                Ok(None) => return Err(invalid("missing attributed cache record")),
                Err(e)
                    if e.is_invalid_store_state_view()
                        || e.is_optional_store_state_unavailable() => {}
                Err(e) => return Err(e),
            }
        }
        Ok(value)
    }
}
