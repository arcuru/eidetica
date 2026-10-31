//! Cleanup and diagnostic-redaction regression tests for PasswordStore-owned plaintext.
//!
//! Every inspected allocation is synthetic and explicitly registered while it is live. The
//! observer reads a registered, fully initialized allocation immediately before the system
//! allocator frees it; it never reads freed memory or scans other memory.

use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    task::{Context, Poll},
};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use argon2::{Argon2, Params};
use async_trait::async_trait;

use super::*;
use crate::{
    Entry, Instance, NewAccount, Snapshot,
    backend::{
        BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage, RecordRange,
        RecordView, StagingToken, StoreStateRequest, VerificationStatus, database::InMemory,
    },
    entry::ID,
    store::DocStore,
};

/// Allocation observer for explicitly registered synthetic allocations.
pub(super) mod observer {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering::SeqCst},
    };

    const SLOT_COUNT: usize = 64;
    const LIVE: usize = usize::MAX;
    /// Fill byte for registered spare capacity, so the whole allocation is initialized.
    const SPARE_FILL: u8 = 0xa5;

    struct Slot {
        owner: AtomicUsize,
        ptr: AtomicUsize,
        size: AtomicUsize,
        nonzero: AtomicUsize,
    }

    static SLOTS: [Slot; SLOT_COUNT] = [const {
        Slot {
            owner: AtomicUsize::new(0),
            ptr: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            nonzero: AtomicUsize::new(LIVE),
        }
    }; SLOT_COUNT];
    static ACTIVE_WATCHES: AtomicUsize = AtomicUsize::new(0);
    static NEXT_WATCH: AtomicUsize = AtomicUsize::new(1);

    thread_local! {
        static ARMED: Cell<usize> = const { Cell::new(0) };
    }

    struct ObservingAllocator;

    #[global_allocator]
    static ALLOCATOR: ObservingAllocator = ObservingAllocator;

    fn observe(ptr: *mut u8, layout_size: usize) {
        for slot in &SLOTS {
            if slot
                .ptr
                .compare_exchange(ptr as usize, 0, SeqCst, SeqCst)
                .is_ok()
            {
                // Registration initialized every byte of this live allocation.
                let size = slot.size.load(SeqCst).min(layout_size);
                let nonzero = (0..size)
                    // SAFETY: the allocation is still live and initialized for `size` bytes.
                    .filter(|&index| unsafe { ptr.add(index).read_volatile() } != 0)
                    .count();
                slot.nonzero.store(nonzero, SeqCst);
                return;
            }
        }
    }

    fn is_watched(ptr: *mut u8) -> bool {
        SLOTS
            .iter()
            .any(|slot| slot.ptr.load(SeqCst) == ptr as usize)
    }

    unsafe impl GlobalAlloc for ObservingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: delegates allocation with Rust's requested layout.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if ACTIVE_WATCHES.load(SeqCst) != 0 {
                observe(ptr, layout.size());
            }
            // SAFETY: delegates deallocation with Rust's original layout.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if ACTIVE_WATCHES.load(SeqCst) == 0 || !is_watched(ptr) {
                // SAFETY: delegates reallocation with Rust's original layout.
                return unsafe { System.realloc(ptr, layout, new_size) };
            }
            // A watched allocation moves: observe the abandoned allocation before freeing it.
            // SAFETY: the new layout keeps the original alignment and a non-zero size.
            let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
            // SAFETY: allocation, copy and deallocation follow the GlobalAlloc contract.
            unsafe {
                let new_ptr = self.alloc(new_layout);
                if !new_ptr.is_null() {
                    std::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                    self.dealloc(ptr, layout);
                }
                new_ptr
            }
        }
    }

    /// What the observer saw for one registered allocation.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Observation {
        pub(crate) size: usize,
        /// Non-zero bytes at deallocation; `None` while the allocation is still live.
        pub(crate) nonzero: Option<usize>,
    }

    impl Observation {
        pub(crate) fn wiped(&self) -> bool {
            self.nonzero == Some(0)
        }
    }

    /// One test's set of registered allocations; registration is thread-local while armed.
    pub(crate) struct Watch {
        id: usize,
    }

    impl Watch {
        pub(crate) fn start() -> Self {
            let id = NEXT_WATCH.fetch_add(1, SeqCst);
            ARMED.with(|armed| {
                assert_eq!(armed.get(), 0, "nested allocation watch");
                armed.set(id);
            });
            ACTIVE_WATCHES.fetch_add(1, SeqCst);
            Self { id }
        }

        pub(crate) fn string(&self, text: &mut String) {
            // SAFETY: only spare capacity beyond the string's length is written.
            register(self.id, unsafe { text.as_mut_vec() });
        }

        /// Stop watching and return observations in registration order.
        pub(crate) fn finish(self) -> Vec<Observation> {
            let mut observations: Vec<(usize, Observation)> = Vec::new();
            for (index, slot) in SLOTS.iter().enumerate() {
                if slot.owner.load(SeqCst) == self.id {
                    let nonzero = slot.nonzero.load(SeqCst);
                    observations.push((
                        index,
                        Observation {
                            size: slot.size.load(SeqCst),
                            nonzero: (nonzero != LIVE).then_some(nonzero),
                        },
                    ));
                }
            }
            observations.sort_by_key(|(index, _)| *index);
            drop(self);
            observations.into_iter().map(|(_, seen)| seen).collect()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            for slot in &SLOTS {
                if slot.owner.load(SeqCst) == self.id {
                    slot.ptr.store(0, SeqCst);
                    slot.size.store(0, SeqCst);
                    slot.nonzero.store(LIVE, SeqCst);
                    slot.owner.store(0, SeqCst);
                }
            }
            ARMED.with(|armed| armed.set(0));
            ACTIVE_WATCHES.fetch_sub(1, SeqCst);
        }
    }

    fn register(owner: usize, bytes: &mut Vec<u8>) {
        assert!(bytes.capacity() > 0, "cannot watch an unallocated buffer");
        for byte in bytes.spare_capacity_mut() {
            byte.write(SPARE_FILL);
        }
        let slot = SLOTS
            .iter()
            .find(|slot| {
                slot.owner
                    .compare_exchange(0, owner, SeqCst, SeqCst)
                    .is_ok()
            })
            .expect("allocation watch slots exhausted");
        slot.nonzero.store(LIVE, SeqCst);
        slot.size.store(bytes.capacity(), SeqCst);
        slot.ptr.store(bytes.as_ptr() as usize, SeqCst);
    }

    /// Register a PasswordStore-owned plaintext allocation if this thread is watching.
    pub(crate) fn register_owned(bytes: &mut Vec<u8>) {
        let owner = ARMED.with(Cell::get);
        if owner != 0 && bytes.capacity() > 0 {
            register(owner, bytes);
        }
    }
}

use observer::{Observation, Watch};

const SALT: &str = "MDEyMzQ1Njc4OUFCQ0RFRg";
const PASSWORD: &str = "synthetic-password";
const SENTINEL: &str = "SYNTHETIC-SECRET-SENTINEL";

/// A synthetic password whose allocation includes initialized spare capacity.
fn watched_password(watch: &Watch, text: &str) -> String {
    let mut password = String::with_capacity(64);
    password.push_str(text);
    watch.string(&mut password);
    password
}

fn assert_all_wiped(observations: &[Observation], expected: usize) {
    assert_eq!(
        observations.len(),
        expected,
        "unexpected registrations: {observations:?}"
    );
    for observation in observations {
        assert!(observation.wiped(), "not wiped: {observations:?}");
    }
}

fn test_key(password: &str) -> Vec<u8> {
    let params = Params::new(8, 1, 1, Some(32)).unwrap();
    let mut key = vec![0; 32];
    Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), SALT.as_bytes(), &mut key)
        .unwrap();
    key
}

/// Configuration whose wrapped metadata decrypts to `plaintext` under `PASSWORD`.
fn crafted_config(plaintext: &[u8], salt: &str, nonce: Vec<u8>) -> Doc {
    let cipher = Aes256Gcm::new_from_slice(&test_key(PASSWORD)).unwrap();
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&[7; AES_GCM_NONCE_SIZE]), plaintext)
        .unwrap();
    PasswordStoreConfig {
        encryption: EncryptionInfo {
            algorithm: "aes-256-gcm".to_string(),
            kdf: "argon2id".to_string(),
            salt: salt.to_string(),
            version: "v0".to_string(),
            argon2_m_cost: Some(8),
            argon2_t_cost: Some(1),
            argon2_p_cost: Some(1),
        },
        wrapped_config: EncryptedFragment { ciphertext, nonce },
    }
    .into()
}

fn valid_wrapped(type_id: &str) -> Vec<u8> {
    format!(r#"{{"type":"{type_id}","config":{{"children":{{}}}}}}"#).into_bytes()
}

async fn database() -> (Instance, crate::Database) {
    database_on(Box::new(InMemory::new())).await
}

async fn database_on(backend: Box<dyn BackendImpl>) -> (Instance, crate::Database) {
    let (instance, mut owner) =
        Instance::create_backend(backend, NewAccount::passwordless("owner"))
            .await
            .unwrap();
    let key = owner.get_default_key().unwrap();
    let db = owner.create_database(Doc::new(), &key).await.unwrap();
    (instance, db)
}

async fn write_config(db: &crate::Database, config: Doc) {
    let tx = db.new_transaction().await.unwrap();
    tx.get_index()
        .await
        .unwrap()
        .set_entry(
            "secrets",
            <PasswordStore<DocStore> as Registered>::type_id(),
            config,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn open_with<S: Store>(db: &crate::Database, password: String) -> Result<()> {
    let tx = db.new_transaction().await.unwrap();
    let mut store = tx.get_store::<PasswordStore<S>>("secrets").await.unwrap();
    store.open(password)
}

fn formatted_chain(error: &crate::Error) -> String {
    let mut text = format!("{error}\n{error:?}");
    let mut source = std::error::Error::source(error);
    while let Some(next) = source {
        text.push_str(&format!("\n{next}\n{next:?}"));
        source = next.source();
    }
    text
}

// ---------------------------------------------------------------------------
// Observer controls
// ---------------------------------------------------------------------------

#[test]
fn observer_detects_plain_drop_and_success_only_wipe() {
    let watch = Watch::start();
    drop(watched_password(&watch, PASSWORD));

    fn wipe_after_fallible_step(mut password: String) -> std::result::Result<(), ()> {
        Err(())?;
        password.zeroize();
        Ok(())
    }
    assert!(wipe_after_fallible_step(watched_password(&watch, PASSWORD)).is_err());

    let observations = watch.finish();
    assert_eq!(observations.len(), 2);
    assert!(
        observations
            .iter()
            .all(|seen| seen.nonzero == Some(seen.size)),
        "plain-owner and success-only controls must stay detectable: {observations:?}"
    );
}

#[test]
fn completed_password_clones_wipe_independently() {
    let watch = Watch::start();
    let original = Password {
        salt: SALT.to_string(),
        password: watched_password(&watch, PASSWORD),
        argon2_m_cost: 8,
        argon2_t_cost: 1,
        argon2_p_cost: 1,
    };
    let mut clone = original.clone();
    watch.string(&mut clone.password);
    drop(original);
    assert_eq!(clone.password, PASSWORD, "clone survives original wipe");
    drop(clone);
    assert_all_wiped(&watch.finish(), 2);
}

// ---------------------------------------------------------------------------
// S1: converted password ownership
// ---------------------------------------------------------------------------

#[tokio::test]
async fn open_failures_wipe_converted_password() {
    // Registrations: the caller's password, the temporary key, and any decrypted JSON.
    let cases: Vec<(&str, Doc, &str, usize)> = vec![
        (
            "wrong password",
            crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 12]),
            "wrong-synthetic-password",
            2,
        ),
        (
            "invalid salt",
            crafted_config(&valid_wrapped("docstore:v0"), "!", vec![7; 12]),
            PASSWORD,
            2,
        ),
        (
            "invalid nonce",
            crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 11]),
            PASSWORD,
            2,
        ),
        (
            "malformed metadata",
            crafted_config(b"not json", SALT, vec![7; 12]),
            PASSWORD,
            3,
        ),
        (
            "type mismatch",
            crafted_config(&valid_wrapped("other:v0"), SALT, vec![7; 12]),
            PASSWORD,
            3,
        ),
    ];
    for (case, config, password, registrations) in cases {
        let (_instance, db) = database().await;
        write_config(&db, config).await;
        let watch = Watch::start();
        let result = open_with::<DocStore>(&db, watched_password(&watch, password)).await;
        assert!(result.is_err(), "{case} must fail");
        let observations = watch.finish();
        assert_eq!(
            observations.len(),
            registrations,
            "{case}: {observations:?}"
        );
        assert_eq!(observations[1].size, 32, "{case}: temporary key guarded");
        assert!(
            observations.iter().all(Observation::wiped),
            "{case}: {observations:?}"
        );
    }
}

#[tokio::test]
async fn opened_store_password_wipes_when_last_owner_drops() {
    let (instance, db) = database().await;
    write_config(
        &db,
        crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 12]),
    )
    .await;
    let watch = Watch::start();
    {
        let tx = db.new_transaction().await.unwrap();
        let mut store = tx
            .get_store::<PasswordStore<DocStore>>("secrets")
            .await
            .unwrap();
        store.open(watched_password(&watch, PASSWORD)).unwrap();
        let inner = store.inner().await.unwrap();
        inner.set("key", "value").await.unwrap();
        drop(inner);
        drop(store);
        tx.commit().await.unwrap();
    }
    drop(db);
    drop(instance);
    let observations = watch.finish();
    assert!(
        !observations.is_empty() && observations.iter().all(Observation::wiped),
        "{observations:?}"
    );
}

/// Store type whose type checks panic, to exercise unwinding through PasswordStore.
struct UnwindStore {
    name: String,
    transaction: Transaction,
}

impl Registered for UnwindStore {
    fn type_id() -> &'static str {
        if PANIC_IN_TYPE_ID.with(std::cell::Cell::get) {
            panic!("synthetic unwind from type_id");
        }
        "docstore:v0"
    }

    fn supports_type_id(_: &str) -> bool {
        panic!("synthetic unwind from supports_type_id");
    }
}

thread_local! {
    static PANIC_IN_TYPE_ID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[async_trait]
impl Store for UnwindStore {
    type Data = Doc;

    async fn load(txn: &Transaction, subtree_name: String) -> Result<Self> {
        Ok(Self {
            name: subtree_name,
            transaction: txn.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.transaction
    }
}

#[tokio::test]
async fn panic_unwind_wipes_converted_password() {
    let (_instance, db) = database().await;
    write_config(
        &db,
        crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 12]),
    )
    .await;
    let tx = db.new_transaction().await.unwrap();
    let mut locked = tx
        .get_store::<PasswordStore<UnwindStore>>("secrets")
        .await
        .unwrap();
    let watch = Watch::start();
    let password = watched_password(&watch, PASSWORD);
    let unwound = catch_unwind(AssertUnwindSafe(|| locked.open(password)));
    assert!(unwound.is_err(), "open must unwind");
    let observations = watch.finish();
    assert!(
        !observations.is_empty() && observations.iter().all(Observation::wiped),
        "open unwind: {observations:?}"
    );

    let mut fresh = tx
        .get_store::<PasswordStore<UnwindStore>>("fresh")
        .await
        .unwrap();
    let watch = Watch::start();
    let password = watched_password(&watch, PASSWORD);
    PANIC_IN_TYPE_ID.with(|flag| flag.set(true));
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let mut initialize = std::pin::pin!(fresh.initialize(password, Doc::new()));
        initialize
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
    }));
    PANIC_IN_TYPE_ID.with(|flag| flag.set(false));
    assert!(unwound.is_err(), "initialize must unwind");
    let observations = watch.finish();
    assert!(
        !observations.is_empty() && observations.iter().all(Observation::wiped),
        "initialize unwind: {observations:?}"
    );
}

/// Persistence gate: open, pending forever, or failing.
const GATE_OPEN: u8 = 0;
const GATE_PENDING: u8 = 1;
const GATE_FAIL: u8 = 2;

/// Backend wrapper that can hold or fail every backend operation.
struct GatedBackend {
    inner: InMemory,
    gate: Arc<AtomicU8>,
}

impl GatedBackend {
    async fn pass(&self) -> Result<()> {
        match self.gate.load(Ordering::SeqCst) {
            GATE_OPEN => Ok(()),
            GATE_FAIL => Err(std::io::Error::other("synthetic persistence failure").into()),
            _ => std::future::pending().await,
        }
    }
}

#[async_trait]
impl BackendImpl for GatedBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }
    async fn put(&self, entry: Entry) -> Result<()> {
        self.pass().await?;
        self.inner.put(entry).await
    }
    async fn update_verification_status(&self, id: &ID, status: VerificationStatus) -> Result<()> {
        self.pass().await?;
        self.inner.update_verification_status(id, status).await
    }
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.pass().await?;
        self.inner.get(id).await
    }
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.pass().await?;
        self.inner.get_verification_status(id).await
    }
    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.pass().await?;
        self.inner.get_entries_by_verification_status(status).await
    }
    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.pass().await?;
        self.inner.snapshot(tree).await
    }
    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.pass().await?;
        self.inner.store_snapshot(tree, store).await
    }
    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.pass().await?;
        self.inner
            .store_snapshot_at(tree, store, main_snapshot)
            .await
    }
    async fn all_roots(&self) -> Result<Vec<ID>> {
        self.pass().await?;
        self.inner.all_roots().await
    }
    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        self.pass().await?;
        self.inner.find_merge_base(tree, store, entry_ids).await
    }
    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.pass().await?;
        self.inner.get_tree(tree).await
    }
    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.pass().await?;
        self.inner.get_store(tree, store).await
    }
    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.pass().await?;
        self.inner.get_tree_from_tips(tree, tips).await
    }
    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.pass().await?;
        self.inner.store_at(tree, store, snapshot).await
    }
    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.pass().await?;
        self.inner
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }
    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.pass().await?;
        self.inner
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }
    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.pass().await?;
        self.inner.get_instance_metadata().await
    }
    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.pass().await?;
        self.inner.set_instance_metadata(metadata).await
    }
    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        self.pass().await?;
        self.inner.get_instance_secrets().await
    }
    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        self.pass().await?;
        self.inner.set_instance_secrets(secrets).await
    }
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        self.pass().await?;
        self.inner.resolve_store_state(request).await
    }
    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        self.pass().await?;
        self.inner.begin_store_state_staging(request).await
    }
    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        self.pass().await?;
        self.inner.stage_store_state_records(token, records).await
    }
    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        self.pass().await?;
        self.inner.publish_store_state(token).await
    }
    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        self.pass().await?;
        self.inner.abort_store_state(token).await
    }
    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.pass().await?;
        self.inner.store_state_record_get(view, key).await
    }
    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        self.pass().await?;
        self.inner
            .store_state_record_scan(view, range, after, limit)
            .await
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        self.pass().await?;
        self.inner.clear_derived_store_state().await
    }
}

/// Run `initialize` until its persistence step reaches the gate.
///
/// Returns the observations after the pending future is cancelled, or the result after a
/// failing gate.
async fn initialize_at_gate(mode: u8) -> (Option<Result<()>>, Vec<Observation>) {
    let gate = Arc::new(AtomicU8::new(GATE_OPEN));
    let (_instance, db) = database_on(Box::new(GatedBackend {
        inner: InMemory::new(),
        gate: gate.clone(),
    }))
    .await;
    let tx = db.new_transaction().await.unwrap();
    // A handle built directly leaves `_index` unread by this transaction, so persistence
    // performs a genuine backend read that the gate can hold. The exhaustive literal also
    // pins that the handle retains no decrypted wrapped metadata.
    let mut store = PasswordStore::<DocStore> {
        name: "secrets".to_string(),
        transaction: tx.clone(),
        config: None,
        cached_password: None,
        _phantom: PhantomData,
    };
    let watch = Watch::start();
    let password = watched_password(&watch, PASSWORD);
    gate.store(mode, Ordering::SeqCst);
    let result = {
        let mut initialize = std::pin::pin!(store.initialize(password, Doc::new()));
        match initialize
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
        {
            Poll::Ready(result) => Some(result),
            Poll::Pending => None,
        }
        // A pending future is cancelled here.
    };
    gate.store(GATE_OPEN, Ordering::SeqCst);
    (result, watch.finish())
}

#[tokio::test]
async fn initialize_cancelled_during_persistence_wipes_password() {
    let (result, observations) = initialize_at_gate(GATE_PENDING).await;
    assert!(
        result.is_none(),
        "initialize must be pending on persistence"
    );
    assert!(
        !observations.is_empty() && observations.iter().all(Observation::wiped),
        "{observations:?}"
    );
}

#[tokio::test]
async fn initialize_persistence_failure_wipes_password() {
    let (result, observations) = initialize_at_gate(GATE_FAIL).await;
    assert!(
        matches!(result, Some(Err(_))),
        "persistence failure must surface"
    );
    assert!(
        !observations.is_empty() && observations.iter().all(Observation::wiped),
        "{observations:?}"
    );
}

// ---------------------------------------------------------------------------
// Redacted decrypted diagnostics
// ---------------------------------------------------------------------------

/// Malformed decrypted metadata is reported generically, whatever the wrapped Store type.
async fn assert_malformed_metadata_redacted<S: Store>() {
    let malformed = format!(r#"{{"type":"{}","config":"{SENTINEL}"}}"#, S::type_id()).into_bytes();
    let (_instance, db) = database().await;
    write_config(&db, crafted_config(&malformed, SALT, vec![7; 12])).await;
    let error = open_with::<S>(&db, PASSWORD.to_string()).await.unwrap_err();
    assert!(error.is_store_serialization_error(), "{error}");
    assert!(!formatted_chain(&error).contains(SENTINEL));
}

/// A decrypted wrapped type that does not match `S` is not echoed in the error.
async fn assert_type_mismatch_redacted<S: Store>() {
    let (_instance, db) = database().await;
    write_config(
        &db,
        crafted_config(&valid_wrapped(SENTINEL), SALT, vec![7; 12]),
    )
    .await;
    let error = open_with::<S>(&db, PASSWORD.to_string()).await.unwrap_err();
    assert!(error.is_store_type_error(), "{error}");
    let chain = formatted_chain(&error);
    assert!(!chain.contains(SENTINEL), "{chain}");
    assert!(
        chain.contains(S::type_id()),
        "expected type stays visible: {chain}"
    );
}

#[tokio::test]
async fn malformed_decrypted_metadata_errors_omit_plaintext() {
    assert_malformed_metadata_redacted::<DocStore>().await;
    assert_malformed_metadata_redacted::<crate::store::Table<String>>().await;
    #[cfg(feature = "y-crdt")]
    assert_malformed_metadata_redacted::<crate::store::YDoc>().await;
}

#[tokio::test]
async fn decrypted_type_mismatch_errors_omit_plaintext_type() {
    assert_type_mismatch_redacted::<DocStore>().await;
    assert_type_mismatch_redacted::<crate::store::Table<String>>().await;
    #[cfg(feature = "y-crdt")]
    assert_type_mismatch_redacted::<crate::store::YDoc>().await;
}

fn record_encryptor() -> PasswordEncryptor {
    PasswordEncryptor::new(
        Password {
            salt: SALT.to_string(),
            password: PASSWORD.to_string(),
            argon2_m_cost: 8,
            argon2_t_cost: 1,
            argon2_p_cost: 1,
        },
        "first".to_string(),
        b"database",
    )
}

/// Encrypt arbitrary envelope plaintext for a row under the encryptor's real AAD.
fn crafted_record(encryptor: &PasswordEncryptor, physical_key: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let key = encryptor.record_value_material().unwrap();
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).unwrap();
    let nonce = [9; AES_GCM_NONCE_SIZE];
    let aad = physical_record_aad(&encryptor.store_identity, physical_key);
    let mut record = nonce.to_vec();
    record.extend(
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .unwrap(),
    );
    record
}

#[test]
fn malformed_decrypted_record_errors_omit_plaintext() {
    let encryptor = record_encryptor();
    let physical_key = encryptor.physical_record_key(b"logical").unwrap();
    let malformed = format!(r#"{{"key":[108],"value":"{SENTINEL}"}}"#);
    let record = crafted_record(&encryptor, &physical_key, malformed.as_bytes());
    let error = encryptor
        .decrypt_record(&physical_key, &record)
        .unwrap_err();
    assert!(error.is_store_serialization_error(), "{error}");
    assert!(!formatted_chain(&error).contains(SENTINEL));
}

// ---------------------------------------------------------------------------
// Persistent-format compatibility fixtures, produced by the format before this change
// ---------------------------------------------------------------------------

const RECORD_FIXTURE: &str = "57e4b5dcc61e9db1b0fef8185b7c4b86819b3fe59c556f30a5786d152f11f12cdd3abb699fa73dd21bb5d51f3b73070c3c9d8fc2c60aa12b791b2d6e65111fbe061d951a6fd72f83bffa6a11d0ae510249e5eebf3c49e0313a2b23afdef55fed7939d0b018494abc1abbf5e3";

#[test]
fn existing_record_fixture_still_decrypts() {
    let encryptor = record_encryptor_for_fixture();
    let physical_key = encryptor.physical_record_key(b"logical").unwrap();
    let (key, value) = encryptor
        .decrypt_record(&physical_key, &hex::decode(RECORD_FIXTURE).unwrap())
        .unwrap();
    assert_eq!(
        (key.as_slice(), value.as_slice()),
        (&b"logical"[..], &b"plaintext"[..])
    );
}

/// The encryptor used when the record fixture was produced.
fn record_encryptor_for_fixture() -> PasswordEncryptor {
    PasswordEncryptor::new(
        Password {
            salt: SALT.to_string(),
            password: "record-authentication".to_string(),
            argon2_m_cost: 8,
            argon2_t_cost: 1,
            argon2_p_cost: 1,
        },
        "first".to_string(),
        b"database",
    )
}

/// Base64 salt text of the config fixture, hex-encoded so the spell checker leaves it intact.
const CONFIG_FIXTURE_SALT_HEX: &str = "734d5533664476307a43775a53653162766535704641";

#[tokio::test]
async fn existing_initialized_config_fixture_still_opens() {
    let config: Doc = PasswordStoreConfig {
        encryption: EncryptionInfo {
            algorithm: "aes-256-gcm".to_string(),
            kdf: "argon2id".to_string(),
            salt: String::from_utf8(hex::decode(CONFIG_FIXTURE_SALT_HEX).unwrap()).unwrap(),
            version: "v0".to_string(),
            argon2_m_cost: Some(DEFAULT_ARGON2_M_COST),
            argon2_t_cost: Some(DEFAULT_ARGON2_T_COST),
            argon2_p_cost: Some(DEFAULT_ARGON2_P_COST),
        },
        wrapped_config: EncryptedFragment {
            ciphertext: hex::decode(
                "cf53da3c62ef3c569f6f8ede5d44fce5c0340f09c1aea1906f26b71b01412c618be9c306273e8f4ae0f418b0956f344bba7204ff1984b9e2340de467a3bf6f94f9cbb58bac40bdbb71162a3dd2f03f805a7a1958099f",
            )
            .unwrap(),
            nonce: hex::decode("f049d9fdd231d28a8f3bf567").unwrap(),
        },
    }
    .into();
    let (_instance, db) = database().await;
    write_config(&db, config).await;
    let tx = db.new_transaction().await.unwrap();
    let mut store = tx
        .get_store::<PasswordStore<DocStore>>("secrets")
        .await
        .unwrap();
    store.open("fixture-password").unwrap();
    let inner = store.inner().await.unwrap();
    inner.set("usable", "yes").await.unwrap();
    assert_eq!(inner.get_string("usable").await.unwrap(), "yes");
}

// ---------------------------------------------------------------------------
// S2: guarded local JSON, crypto output and completed row fields
// ---------------------------------------------------------------------------

#[test]
fn lazy_key_derivation_guards_and_moves_its_buffer() {
    let watch = Watch::start();
    let encryptor = record_encryptor();
    encryptor.with_key(|key| Ok(key.len())).unwrap();
    encryptor.with_key(|key| Ok(key.len())).unwrap();
    let live = watch_snapshot_is_live(&encryptor);
    drop(encryptor);
    let observations = watch.finish();
    assert!(live, "derived key stays cached while its encryptor lives");
    assert_all_wiped(&observations, 1);
    assert_eq!(observations[0].size, 32);

    let watch = Watch::start();
    let mut invalid_salt = record_encryptor();
    invalid_salt.password.salt = "!".to_string();
    assert!(invalid_salt.with_key(|key| Ok(key.len())).is_err());
    drop(invalid_salt);
    assert_all_wiped(&watch.finish(), 1);
}

/// The cached key is the moved derivation buffer, not a copy.
fn watch_snapshot_is_live(encryptor: &PasswordEncryptor) -> bool {
    encryptor.derived_key.lock().unwrap().get().is_some()
}

#[test]
fn secret_bytes_transfer_is_zero_copy_and_unwiped() {
    let watch = Watch::start();
    let owner = SecretBytes::from_vec(b"synthetic".to_vec());
    let ptr = owner.as_bytes().as_ptr();
    let transferred = owner.into_vec();
    assert_eq!(transferred.as_ptr(), ptr, "transfer must not copy");
    assert_eq!(transferred, b"synthetic");
    let observations = watch.finish();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].nonzero, None,
        "transferred bytes are not wiped"
    );
}

/// Serializes successfully on the measuring pass and fails part-way on the next.
struct FailsOnSecondPass(std::cell::Cell<usize>);

impl Serialize for FailsOnSecondPass {
    fn serialize<Z: serde::Serializer>(
        &self,
        serializer: Z,
    ) -> std::result::Result<Z::Ok, Z::Error> {
        use serde::ser::SerializeStruct;
        let pass = self.0.get();
        self.0.set(pass + 1);
        let mut state = serializer.serialize_struct("Synthetic", 2)?;
        state.serialize_field("token", SENTINEL)?;
        if pass > 0 {
            return Err(serde::ser::Error::custom("synthetic serializer failure"));
        }
        state.serialize_field("tail", "x")?;
        state.end()
    }
}

#[test]
fn guarded_json_writer_wipes_partial_output_on_serialization_error() {
    let watch = Watch::start();
    let value = FailsOnSecondPass(std::cell::Cell::new(0));
    assert!(SecretBytes::json(&value).is_err());
    assert_eq!(value.0.get(), 2, "the guarded pass must run and fail");
    assert_all_wiped(&watch.finish(), 1);
}

#[test]
fn guarded_json_is_allocated_once_at_its_final_size() {
    let mut config = Doc::new();
    config.set("title", "Test");
    let watch = Watch::start();
    let json = SecretBytes::json(&WrappedStoreInfoRef {
        type_id: "docstore:v0",
        config: &config,
    })
    .unwrap();
    assert_eq!(json.0.capacity(), json.as_bytes().len());
    drop(json);
    assert_all_wiped(&watch.finish(), 1);
}

#[test]
fn record_encryption_wipes_its_envelope_json() {
    let encryptor = record_encryptor();
    // Derive the cached key first, so only the envelope is registered.
    encryptor.with_key(|key| Ok(key.len())).unwrap();
    let watch = Watch::start();
    encryptor.encrypt_record(b"logical", b"plaintext").unwrap();
    assert_all_wiped(&watch.finish(), 1);
}

#[test]
fn record_decryption_transfers_accepted_fields_and_wipes_json() {
    let encryptor = record_encryptor();
    let physical_key = encryptor.physical_record_key(b"logical").unwrap();
    let record = encryptor.encrypt_record(b"logical", b"plaintext").unwrap();
    let watch = Watch::start();
    let (key, value) = encryptor.decrypt_record(&physical_key, &record).unwrap();
    let observations = watch.finish();
    // JSON, then the two completed fields.
    assert_eq!(observations.len(), 3, "{observations:?}");
    assert!(observations[0].wiped(), "envelope JSON: {observations:?}");
    assert_eq!(
        (observations[1].nonzero, observations[2].nonzero),
        (None, None),
        "accepted fields are transferred, not wiped or copied"
    );
    assert_eq!(
        (key.as_slice(), value.as_slice()),
        (&b"logical"[..], &b"plaintext"[..])
    );
}

#[test]
fn record_key_mismatch_wipes_completed_fields() {
    let encryptor = record_encryptor();
    let physical_key = encryptor.physical_record_key(b"logical").unwrap();
    let envelope = SecretBytes::json(&EncryptedRecordEnvelope {
        key: b"other",
        value: b"plaintext",
    })
    .unwrap();
    let record = crafted_record(&encryptor, &physical_key, envelope.as_bytes());
    let watch = Watch::start();
    let error = encryptor
        .decrypt_record(&physical_key, &record)
        .unwrap_err();
    assert!(
        matches!(&error, crate::Error::Store(store) if store.is_integrity_error()),
        "{error}"
    );
    assert_all_wiped(&watch.finish(), 3);
}

#[test]
fn malformed_record_wipes_completed_fields_but_not_partial_ones() {
    let encryptor = record_encryptor();
    let physical_key = encryptor.physical_record_key(b"logical").unwrap();
    // `key` completes; `value` fails part-way through its array.
    let record = crafted_record(
        &encryptor,
        &physical_key,
        br#"{"key":[108,111],"value":[1,2,"x"]}"#,
    );
    let watch = Watch::start();
    assert!(encryptor.decrypt_record(&physical_key, &record).is_err());
    // Registered: the JSON and the completed `key`. The partially built `value` vector is
    // serde's ordinary allocation and is outside the cleanup claim.
    assert_all_wiped(&watch.finish(), 2);
}

#[test]
fn record_envelope_json_bytes_are_unchanged() {
    let json = SecretBytes::json(&EncryptedRecordEnvelope {
        key: b"logical",
        value: b"plaintext",
    })
    .unwrap();
    assert_eq!(
        json.as_bytes(),
        br#"{"key":[108,111,103,105,99,97,108],"value":[112,108,97,105,110,116,101,120,116]}"#
    );
}

#[test]
fn wrapped_metadata_json_bytes_are_unchanged() {
    let mut config = Doc::new();
    config.set("title", "Test");
    let json = SecretBytes::json(&WrappedStoreInfoRef {
        type_id: "docstore:v0",
        config: &config,
    })
    .unwrap();
    assert_eq!(
        json.as_bytes(),
        br#"{"type":"docstore:v0","config":{"children":{"title":{"Text":"Test"}}}}"#
    );
}

#[test]
fn fixed_nonce_record_encryption_matches_existing_fixture() {
    let fixture = hex::decode(RECORD_FIXTURE).unwrap();
    let encryptor = record_encryptor_for_fixture();
    let ciphertext = encryptor
        .encrypt_record_with_nonce(b"logical", b"plaintext", &fixture[..AES_GCM_NONCE_SIZE])
        .unwrap();
    assert_eq!(ciphertext, fixture);
}

// ---------------------------------------------------------------------------
// Output-trait guards: these fail to compile if a secret owner gains an output trait
// ---------------------------------------------------------------------------

macro_rules! assert_not_impl {
    ($ty:ty, $($trait:path),+ $(,)?) => {
        $(
            const _: fn() = || {
                trait AmbiguousIfImpl<A> {
                    fn some_item() {}
                }
                impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
                #[allow(dead_code)]
                struct Invalid;
                impl<T: ?Sized + $trait> AmbiguousIfImpl<Invalid> for T {}
                let _ = <$ty as AmbiguousIfImpl<_>>::some_item;
            };
        )+
    };
}

assert_not_impl!(
    SecretBytes,
    std::fmt::Debug,
    std::fmt::Display,
    Clone,
    Serialize,
    serde::de::DeserializeOwned,
);
assert_not_impl!(Password, std::fmt::Debug, std::fmt::Display, Serialize);
assert_not_impl!(DerivedKey, std::fmt::Debug, Clone, Serialize);
assert_not_impl!(DecryptedRecordEnvelope, std::fmt::Debug, Clone, Serialize);
assert_not_impl!(WrappedStoreInfo, std::fmt::Debug, Clone, Serialize);

#[test]
fn secret_owners_still_allow_local_borrowing() {
    let mut owner = SecretBytes::zeroed(4);
    owner.as_mut_bytes().copy_from_slice(b"abcd");
    let borrowed: &[u8] = owner.as_bytes();
    assert_eq!(borrowed, b"abcd");
    let password = Password {
        salt: SALT.to_string(),
        password: PASSWORD.to_string(),
        argon2_m_cost: 8,
        argon2_t_cost: 1,
        argon2_p_cost: 1,
    };
    assert_eq!(password.password.as_str(), PASSWORD);
}
