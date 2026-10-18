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
    #[allow(dead_code)]
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
    let cases: Vec<(&str, Doc, &str)> = vec![
        (
            "wrong password",
            crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 12]),
            "wrong-synthetic-password",
        ),
        (
            "invalid salt",
            crafted_config(&valid_wrapped("docstore:v0"), "!", vec![7; 12]),
            PASSWORD,
        ),
        (
            "invalid nonce",
            crafted_config(&valid_wrapped("docstore:v0"), SALT, vec![7; 11]),
            PASSWORD,
        ),
        (
            "malformed metadata",
            crafted_config(b"not json", SALT, vec![7; 12]),
            PASSWORD,
        ),
        (
            "type mismatch",
            crafted_config(&valid_wrapped("other:v0"), SALT, vec![7; 12]),
            PASSWORD,
        ),
    ];
    for (case, config, password) in cases {
        let (_instance, db) = database().await;
        write_config(&db, config).await;
        let watch = Watch::start();
        let result = open_with::<DocStore>(&db, watched_password(&watch, password)).await;
        assert!(result.is_err(), "{case} must fail");
        let observations = watch.finish();
        assert!(
            !observations.is_empty() && observations.iter().all(Observation::wiped),
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
    // performs a genuine backend read that the gate can hold.
    let mut store = PasswordStore::<DocStore> {
        name: "secrets".to_string(),
        transaction: tx.clone(),
        config: None,
        cached_password: None,
        wrapped_info: None,
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

#[tokio::test]
async fn malformed_decrypted_metadata_errors_omit_plaintext() {
    let malformed = format!(r#"{{"type":"docstore:v0","config":"{SENTINEL}"}}"#).into_bytes();
    let (_instance, db) = database().await;
    write_config(&db, crafted_config(&malformed, SALT, vec![7; 12])).await;
    let error = open_with::<DocStore>(&db, PASSWORD.to_string())
        .await
        .unwrap_err();
    assert!(error.is_store_serialization_error(), "{error}");
    assert!(!formatted_chain(&error).contains(SENTINEL));
}

#[tokio::test]
async fn decrypted_type_mismatch_errors_omit_plaintext_type() {
    let (_instance, db) = database().await;
    write_config(
        &db,
        crafted_config(&valid_wrapped(SENTINEL), SALT, vec![7; 12]),
    )
    .await;
    let error = open_with::<DocStore>(&db, PASSWORD.to_string())
        .await
        .unwrap_err();
    assert!(error.is_store_type_error(), "{error}");
    assert!(!formatted_chain(&error).contains(SENTINEL));
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

#[tokio::test]
async fn existing_initialized_config_fixture_still_opens() {
    let config: Doc = PasswordStoreConfig {
        encryption: EncryptionInfo {
            algorithm: "aes-256-gcm".to_string(),
            kdf: "argon2id".to_string(),
            salt: "sMU3fDv0zCwZSe1bve5pFA".to_string(),
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
