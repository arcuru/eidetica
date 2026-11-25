//! Historical attribution is evaluated in the signed main DAG. Store links
//! determine replay order and cache boundaries, never the declaration frontier.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
};

use super::{Transaction, TransactionError};
use crate::{
    Entry, Error, Result, Snapshot,
    backend::{CacheScope, VerificationStatus},
    constants::INDEX,
    crdt::{CRDT, Codec, Doc, doc::Value},
    entry::{EntryBuilder, ID},
    store::{
        Store, StoreError, StructuredStore,
        attribution::{self, ConfigState, RowDelta, TypeEvent},
        schema::{ContributionType, StructuredState, StructuredType},
        state,
    },
};

struct MainHistory {
    store: String,
    entries: Vec<Entry>,
    by_id: HashMap<ID, usize>,
    parents: Vec<Vec<usize>>,
}

impl MainHistory {
    async fn load(tx: &Transaction, store: &str, tips: &[ID]) -> Result<Self> {
        let invalid = |reason: &str| unavailable(store, reason);
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
                        let entry = checked_entry(tx, store, &id).await?;
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
            store: store.into(),
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
                    let entry = checked_entry(tx, store, &id).await?;
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
        self.by_id.get(id).copied().ok_or_else(|| {
            unavailable(
                &self.store,
                "Entry is outside the captured verified history",
            )
        })
    }

    fn positions(&self, ids: &[ID]) -> Result<Vec<usize>> {
        ids.iter().map(|id| self.position(id)).collect()
    }

    fn contribution(&self, registration: Registration) -> ContributionType {
        ContributionType {
            type_id: registration.type_id,
            declared_by: registration
                .declared_by
                .into_iter()
                .map(|n| self.entries[n].id())
                .collect(),
        }
    }

    fn store_order(&self, tips: &[ID]) -> Result<Vec<usize>> {
        let mut seen = HashSet::new();
        let mut pending = tips.to_vec();
        while let Some(id) = pending.pop() {
            let n = self.position(&id)?;
            if seen.insert(n) {
                pending.extend(self.entries[n].subtree_parents(&self.store)?);
            }
        }
        let mut order = seen
            .into_iter()
            .map(|n| {
                Ok((
                    self.entries[n].subtree_height(&self.store)?,
                    self.entries[n].id_ref(),
                    n,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        order.sort_unstable();
        Ok(order.into_iter().map(|(_, _, n)| n).collect())
    }

    fn store_tips(&self) -> Result<Snapshot> {
        let mut tips = BTreeSet::new();
        for entry in &self.entries {
            if entry.in_subtree(&self.store) {
                tips.insert(entry.id());
                for p in entry.subtree_parents(&self.store)? {
                    // Never treat an omitted/missing dependency as a branch.
                    if !self.entries[self.position(&p)?].in_subtree(&self.store) {
                        return Err(unavailable(
                            &self.store,
                            "a Store parent does not contribute to the Store",
                        ));
                    }
                    tips.remove(&p);
                }
            }
        }
        Ok(Snapshot::from(tips.into_iter().collect::<Vec<_>>()))
    }
}

fn unavailable(store: &str, reason: &str) -> Error {
    StoreError::HistoryUnavailable {
        store: store.into(),
        reason: reason.into(),
    }
    .into()
}

fn unresolved(store: &str, reason: &str) -> Error {
    StoreError::UnresolvedType {
        store: store.into(),
        reason: reason.into(),
    }
    .into()
}

async fn checked_entry(tx: &Transaction, store: &str, id: &ID) -> Result<Entry> {
    let entry = tx.db.ops().get(id).await?;
    if entry.id_ref() != id || !entry.in_tree(tx.db.root_id()) {
        return Err(unavailable(store, "wrong source Entry"));
    }
    Ok(entry)
}

/// Surviving declarations of one Store at one cut. The two halves move
/// independently: a config edit does not declare a Type.
#[derive(Clone, Default)]
struct Frontier {
    types: BTreeSet<usize>,
    configs: BTreeSet<usize>,
}

struct Registrations<'a> {
    history: &'a MainHistory,
    types: HashMap<usize, TypeEvent>,
    configs: HashMap<usize, Vec<ConfigState>>,
    frontiers: Vec<Frontier>,
    reachability: HashMap<(usize, usize), bool>,
}

/// One cut's unique Type, and every distinct config its history leaves there.
struct Registration {
    type_id: String,
    declared_by: Vec<usize>,
    configs: Vec<ConfigState>,
}

impl<'a> Registrations<'a> {
    fn new(history: &'a MainHistory) -> Self {
        let mut registrations = Self {
            history,
            types: HashMap::new(),
            configs: HashMap::new(),
            frontiers: Vec::with_capacity(history.entries.len()),
            reachability: HashMap::new(),
        };
        for (n, entry) in history.entries.iter().enumerate() {
            let frontier = registrations.step(
                &history.parents[n],
                n,
                entry.data(INDEX).ok().map(Vec::as_slice),
            );
            registrations.frontiers.push(frontier);
        }
        registrations
    }

    /// The registration an Entry with these main parents and row delta would have.
    fn staged(&mut self, parents: &[usize], index: Option<&[u8]>) -> Result<Registration> {
        // One past the history: a position no committed Entry can reach.
        let n = self.history.entries.len();
        let frontier = self.step(parents, n, index);
        self.resolve(&frontier)
    }

    /// Inherited declarations, then this Entry's own row delta last.
    fn step(&mut self, parents: &[usize], n: usize, index: Option<&[u8]>) -> Frontier {
        let mut frontier = self.inherited(parents);
        let delta = index.map_or_else(RowDelta::default, |bytes| {
            attribution::project(bytes, &self.history.store)
        });
        if let Some(event) = delta.type_event {
            self.types.insert(n, event);
            frontier.types = BTreeSet::from([n]);
        }
        if let Some(event) = delta.config {
            let states = distinct(
                self.config_states(&frontier.configs)
                    .iter()
                    .map(|base| base.apply(&event)),
            );
            self.configs.insert(n, states);
            frontier.configs = BTreeSet::from([n]);
        }
        frontier
    }

    fn config_states(&self, origins: &BTreeSet<usize>) -> Vec<ConfigState> {
        if origins.is_empty() {
            return vec![ConfigState::Absent];
        }
        distinct(origins.iter().flat_map(|n| self.configs[n].iter().cloned()))
    }

    fn inherited(&mut self, parents: &[usize]) -> Frontier {
        if let [parent] = parents {
            return self.frontiers[*parent].clone();
        }
        let mut union = Frontier::default();
        for parent in parents {
            union.types.extend(&self.frontiers[*parent].types);
            union.configs.extend(&self.frontiers[*parent].configs);
        }
        Frontier {
            types: self.latest(union.types),
            configs: self.latest(union.configs),
        }
    }

    /// Drop an origin only if a later one descends from it. Parent sets were
    /// already reduced, so this compares origins only at joins. The topological
    /// index is a pruning bound, never a winner rule.
    fn latest(&mut self, candidates: BTreeSet<usize>) -> BTreeSet<usize> {
        let mut latest = BTreeSet::new();
        for origin in candidates.into_iter().rev() {
            if !latest.iter().any(|&later| self.is_ancestor(origin, later)) {
                latest.insert(origin);
            }
        }
        latest
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

    fn at(&self, n: usize) -> Result<Registration> {
        self.resolve(&self.frontiers[n])
    }

    fn resolve(&self, frontier: &Frontier) -> Result<Registration> {
        let store = &self.history.store;
        let mut type_id: Option<&String> = None;
        for origin in &frontier.types {
            let declared = match &self.types[origin] {
                TypeEvent::Declared(declared) => declared,
                TypeEvent::Malformed => return Err(unresolved(store, "malformed registration")),
                // Alone this is an unregistered Store. Beside a declaration it is
                // a conflict. Neither has a Type.
                TypeEvent::Removed => return Err(unresolved(store, "registration was removed")),
            };
            if type_id.is_some_and(|previous| previous != declared) {
                return Err(unresolved(store, "concurrent declarations disagree"));
            }
            type_id = Some(declared);
        }
        Ok(Registration {
            type_id: type_id
                .ok_or_else(|| unresolved(store, "no Type was declared"))?
                .clone(),
            declared_by: frontier.types.iter().copied().collect(),
            configs: self.config_states(&frontier.configs),
        })
    }
}

fn distinct<T: PartialEq>(values: impl Iterator<Item = T>) -> Vec<T> {
    let mut distinct = Vec::new();
    for value in values {
        if !distinct.contains(&value) {
            distinct.push(value);
        }
    }
    distinct
}

/// Bytes an encoder produced, with what it produced them for. Commit compares
/// them with the final declaration instead of trusting the order of API calls.
#[derive(Clone)]
pub(crate) struct StagedContribution {
    type_id: &'static str,
    grammar_of: fn(&[Doc]) -> Result<Doc>,
    grammar: Doc,
    bytes: Vec<u8>,
}

/// Everything a typed handle needs from the captured history.
pub(crate) struct StructuredHistory<T: StructuredType> {
    pub(crate) state: StructuredState<T>,
    /// Grammar shared by all contributions, if there are any.
    pub(crate) grammar: Option<Doc>,
    /// Grammar the next value would be encoded for, or why there is none.
    pub(crate) writer_grammar: Result<Doc>,
}

impl Transaction {
    /// Detach staged state from cloned handles, so nothing can change between the
    /// final Type check and signing.
    pub(super) fn freeze_structured(&mut self) -> Result<bool> {
        let (builder, literals, staged, config_writers, encryptors, record_deltas) = {
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
                self.structured_staging.lock().unwrap().clone(),
                self.structured_config_writers.lock().unwrap().clone(),
                self.encryptors.lock().unwrap().clone(),
                self.logical_record_mutations.lock().unwrap().clone(),
            )
        };
        self.entry_builder = Arc::new(Mutex::new(Some(builder)));
        self.literal_registrations = Arc::new(Mutex::new(literals));
        self.structured_staging = Arc::new(Mutex::new(staged));
        self.structured_config_writers = Arc::new(Mutex::new(config_writers));
        self.encryptors = Arc::new(Mutex::new(encryptors));
        self.logical_record_mutations = Arc::new(Mutex::new(record_deltas));
        Ok(true)
    }

    pub(crate) fn mark_literal_registration(&self, name: &str) -> Result<()> {
        let builder = self.entry_builder.lock().unwrap();
        if builder.is_none() {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        self.literal_registrations
            .lock()
            .unwrap()
            .insert(name.into());
        Ok(())
    }

    /// A config edit is valid only under the Type whose handle made it.
    pub(crate) fn stage_config_writer(&self, name: &str, type_id: &'static str) -> Result<()> {
        let builder = self.entry_builder.lock().unwrap();
        if builder.is_none() {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        self.structured_config_writers
            .lock()
            .unwrap()
            .entry(name.into())
            .or_default()
            .insert(type_id);
        Ok(())
    }

    pub(crate) fn structured_is_encrypted(&self, store: &str) -> bool {
        self.encryptors.lock().unwrap().contains_key(store)
    }

    pub(crate) fn structured_error(&self, store: &str, operation: &str, error: Error) -> Error {
        // Application serializers and mergers can return arbitrary errors, and an
        // inner Type is private. No error text from an encrypted structured
        // operation is safe to echo.
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

    /// Serialize decode/compose/preview/replace across cloned handles.
    ///
    /// `apply` sees the value and grammar staged by this Type's encoder. Bytes
    /// from anything else are never decoded.
    #[allow(clippy::type_complexity)]
    pub(crate) fn with_structured_staging<T: StructuredType, R>(
        &self,
        store: &str,
        apply: impl FnOnce(
            Option<StructuredState<T>>,
            Option<&Doc>,
        ) -> Result<(R, Option<(Vec<u8>, Doc)>)>,
    ) -> Result<R> {
        let mut guard = self.entry_builder.lock().unwrap();
        let builder = guard
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        let mut staging = self.structured_staging.lock().unwrap();
        let current = match builder.data(store).ok() {
            None => None,
            Some(bytes) => Some(
                staging
                    .get(store)
                    .filter(|staged| staged.type_id == T::TYPE_ID && &staged.bytes == bytes)
                    .ok_or_else(|| unresolved(store, "staged bytes are not of this Type"))?,
            ),
        };
        let value = current
            .map(|staged| StructuredState::decode(&staged.bytes))
            .transpose()?;
        let (result, replacement) = apply(value, current.map(|staged| &staged.grammar))?;
        if let Some((bytes, grammar)) = replacement {
            builder.set_subtree_data_mut(store.to_string(), bytes.clone());
            staging.insert(
                store.into(),
                StagedContribution {
                    type_id: T::TYPE_ID,
                    grammar_of: T::grammar,
                    grammar,
                    bytes,
                },
            );
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
        self.mark_literal_registration(store)?;
        let mut guard = self.entry_builder.lock().unwrap();
        let builder = guard
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        let mut rows = builder
            .data(index)
            .ok()
            .map(|bytes| Doc::decode(bytes))
            .transpose()?
            .unwrap_or_default();
        let row = match rows.get_literal(store) {
            Some(Value::Doc(row)) => row.merge(&patch)?,
            _ => patch,
        };
        rows.set_literal(store, Value::Doc(row));
        builder.set_subtree_data_mut(index.to_string(), rows.encode()?);
        Ok(())
    }

    /// Turn the public declaration into the Type that owns the bytes. Only an
    /// unlocked wrapper can open its protected metadata, at this same cut.
    fn interpret(&self, store: &str, outer: &Registration) -> Result<(String, Vec<Doc>)> {
        let configs = outer
            .configs
            .iter()
            .map(|state| match state {
                ConfigState::Present(doc) => Ok(doc.clone()),
                ConfigState::Absent => Ok(Doc::new()),
                ConfigState::Malformed => Err(unresolved(store, "malformed Store config")),
            })
            .collect::<Result<Vec<_>>>()?;
        let encryptors = self.encryptors.lock().unwrap();
        let Some(encryptor) = encryptors.get(store) else {
            return Ok((outer.type_id.clone(), distinct(configs.into_iter())));
        };
        let mut type_id = None;
        let mut inner_configs = Vec::new();
        for config in configs {
            let (inner, config) = encryptor.inner_registration(&outer.type_id, &config)?;
            if type_id.as_ref().is_some_and(|previous| previous != &inner) {
                return Err(unresolved(store, "concurrent declarations disagree"));
            }
            type_id = Some(inner);
            inner_configs.push(config);
        }
        Ok((
            type_id.ok_or_else(|| unresolved(store, "no Type was declared"))?,
            distinct(inner_configs.into_iter()),
        ))
    }

    /// Check every staged structured value against the final declaration of the
    /// Entry that will carry it, before anything is encrypted or signed.
    pub(super) async fn finalize_structured(&self, builder: &EntryBuilder) -> Result<()> {
        let names = self.literal_registrations.lock().unwrap().clone();
        for store in names {
            let writers = self
                .structured_config_writers
                .lock()
                .unwrap()
                .get(&store)
                .cloned()
                .unwrap_or_default();
            let data = builder.data(&store).ok();
            if data.is_none() && writers.is_empty() {
                continue;
            }
            self.check_declaration(builder, &store, data, &writers)
                .await
                .map_err(|e| self.structured_error(&store, "commit", e))?;
        }
        Ok(())
    }

    async fn check_declaration(
        &self,
        builder: &EntryBuilder,
        store: &str,
        data: Option<&Vec<u8>>,
        config_writers: &BTreeSet<&'static str>,
    ) -> Result<()> {
        let parents = builder.parents()?;
        let history = MainHistory::load(self, store, &parents).await?;
        let positions = history.positions(&parents)?;
        let registration = Registrations::new(&history)
            .staged(&positions, builder.data(INDEX).ok().map(Vec::as_slice))?;
        let (type_id, configs) = self.interpret(store, &registration)?;
        if config_writers.iter().any(|writer| *writer != type_id) {
            return Err(unresolved(
                store,
                "config was edited for a different Type than the one declared",
            ));
        }
        let Some(data) = data else {
            return Ok(());
        };
        let staged = self
            .structured_staging
            .lock()
            .unwrap()
            .get(store)
            .filter(|staged| &staged.bytes == data)
            .cloned()
            .ok_or_else(|| unresolved(store, "staged bytes have no encoder Type"))?;
        if staged.type_id != type_id {
            return Err(unresolved(
                store,
                "value was encoded for a different Type than the one declared",
            ));
        }
        if (staged.grammar_of)(&configs)? != staged.grammar {
            return Err(unresolved(
                store,
                "value was encoded for a different Store config than the one declared",
            ));
        }
        Ok(())
    }

    /// Identify the declared Type of one contribution without decoding its value
    /// or needing code for the Type.
    ///
    /// Uses this transaction's captured parents and the handle's read access.
    /// `Ok(None)` means that the Entry has no data for `store`. A wrapped Store
    /// reports its wrapper's Type.
    ///
    /// # Errors
    /// [`StoreError::UnresolvedType`] if the complete verified history gives the
    /// contribution no unique Type. [`StoreError::HistoryUnavailable`] or a
    /// backend error if that history is incomplete, which is not evidence that no
    /// Type was declared.
    pub async fn inspect_contribution(
        &self,
        store: impl AsRef<str> + Send,
        entry: &ID,
    ) -> Result<Option<ContributionType>> {
        Ok(self
            .contribution_registration(store.as_ref(), entry)
            .await?
            .map(|(history, registration)| history.contribution(registration)))
    }

    /// `inspect_contribution` through an unlocked wrapper.
    pub(crate) async fn inspect_wrapped(
        &self,
        store: &str,
        entry: &ID,
    ) -> Result<Option<ContributionType>> {
        async {
            let Some((history, registration)) =
                self.contribution_registration(store, entry).await?
            else {
                return Ok(None);
            };
            let (type_id, _) = self.interpret(store, &registration)?;
            Ok(Some(ContributionType {
                type_id,
                ..history.contribution(registration)
            }))
        }
        .await
        .map_err(|e| self.structured_error(store, "inspect", e))
    }

    async fn contribution_registration(
        &self,
        store: &str,
        entry: &ID,
    ) -> Result<Option<(MainHistory, Registration)>> {
        let history = self.main_history(store).await?;
        let n = history.position(entry)?;
        if history.entries[n].data(store).is_err() {
            return Ok(None);
        }
        let registration = Registrations::new(&history).at(n)?;
        Ok(Some((history, registration)))
    }

    /// Refuse to copy the displayed registry row over concurrent declarations
    /// that disagree. Without a complete verified history there is nothing to
    /// compare, and the legacy registry path keeps its existing behavior.
    pub(crate) async fn check_registry_copy(&self, store: &str) -> Result<()> {
        let Ok(history) = self.main_history(store).await else {
            return Ok(());
        };
        let (parents, index) = {
            let guard = self.entry_builder.lock().unwrap();
            let builder = guard
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            (builder.parents()?, builder.data(INDEX).ok().cloned())
        };
        let positions = history.positions(&parents)?;
        let mut registrations = Registrations::new(&history);
        let n = history.entries.len();
        let frontier = registrations.step(&positions, n, index.as_deref());
        if distinct(frontier.types.iter().map(|n| &registrations.types[n])).len() > 1
            || registrations.config_states(&frontier.configs).len() > 1
        {
            return Err(unresolved(store, "concurrent declarations disagree"));
        }
        Ok(())
    }

    async fn main_history(&self, store: &str) -> Result<MainHistory> {
        let parents = {
            let guard = self.entry_builder.lock().unwrap();
            guard
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?
                .parents()?
        };
        MainHistory::load(self, store, &parents).await
    }

    pub(crate) async fn read_structured<T: StructuredType>(
        &self,
        store: &str,
    ) -> Result<StructuredHistory<T>> {
        self.init_subtree_parents(store).await?;
        let (parents, captured, index) = {
            let guard = self.entry_builder.lock().unwrap();
            let builder = guard
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            (
                builder.parents()?,
                Snapshot::from(builder.subtree_parents(store)?),
                builder.data(INDEX).ok().cloned(),
            )
        };
        let history = MainHistory::load(self, store, &parents).await?;
        if history.store_tips()? != captured {
            return Err(unavailable(
                store,
                "captured Store tips do not cover the verified main history",
            ));
        }
        let mut registrations = Registrations::new(&history);

        // Gate every contribution before a warm cache hit, application
        // deserialization or merge. The cache does not prove source history.
        let mut contributions = Vec::new();
        let mut grammar: Option<Doc> = None;
        for n in history.store_order(captured.tips())? {
            let Ok(data) = history.entries[n].data(store) else {
                continue;
            };
            let (type_id, configs) = self.interpret(store, &registrations.at(n)?)?;
            if type_id != T::TYPE_ID {
                return Err(unresolved(
                    store,
                    "history contains a contribution of an unsupported Type",
                ));
            }
            let found = T::grammar(&configs)?;
            if grammar.as_ref().is_some_and(|previous| previous != &found) {
                return Err(unresolved(
                    store,
                    "history combines incompatible Store configs",
                ));
            }
            grammar = Some(found);
            contributions.push(data);
        }

        // A handle may encode before its Type is declared in this Entry. Commit
        // still requires the final config to mean the same grammar.
        let positions = history.positions(&parents)?;
        let writer_grammar = match registrations
            .staged(&positions, index.as_deref())
            .and_then(|registration| self.interpret(store, &registration))
        {
            Ok((type_id, configs)) if type_id == T::TYPE_ID => T::grammar(&configs),
            _ => T::grammar(&[StructuredStore::<T>::default_config()]),
        };
        let state = if contributions.is_empty() {
            StructuredState::default()
        } else {
            self.fold_structured(store, &captured, contributions)
                .await?
        };
        Ok(StructuredHistory {
            state,
            grammar,
            writer_grammar,
        })
    }

    async fn fold_structured<T: StructuredType>(
        &self,
        store: &str,
        boundary: &Snapshot,
        contributions: Vec<&Vec<u8>>,
    ) -> Result<StructuredState<T>> {
        let request = state::opaque_request(
            self.db.root_id(),
            store,
            self.encrypted_projection_descriptor(
                store,
                StructuredStore::<T>::state_model().descriptor(),
            ),
            boundary.cache_key_bytes(),
            CacheScope::Shared,
        );
        if let Some(bytes) = state::load_cached(self.db.ops(), &request).await? {
            return StructuredState::decode_cached(&self.decrypt_if_needed(store, &bytes)?);
        }
        let mut state = StructuredState::default();
        for bytes in contributions {
            let bytes = self.decrypt_if_needed(store, bytes)?;
            state = state.merge(&StructuredState::decode(&bytes)?)?;
        }
        state::store_cached(self.db.ops(), request, state, |state| {
            self.encrypt_if_needed(store, &state.encode_cached()?)
        })
        .await
    }
}
