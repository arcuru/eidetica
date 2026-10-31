//! Trace the public sync boundary used by bridge session-tree pulls over real HTTP.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;

use super::{
    Address, DatabaseTicket, Sync, SyncError, TimeoutPhase,
    handler::{SyncHandler, SyncHandlerImpl},
    protocol::{RequestContext, SyncRequest, SyncResponse},
    transports::{SyncTransport, http::HttpTransport},
};
use crate::{
    Database, Error, Instance, NewAccount, Result,
    account::types::SyncSettings,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
};

#[derive(Clone, Default)]
pub(super) struct Trace(Arc<Mutex<Vec<u8>>>);

impl io::Write for Trace {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Trace {
    pub(super) fn capture(&self) -> tracing::subscriber::DefaultGuard {
        let writer = self.clone();
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .without_time()
                .with_writer(move || writer.clone())
                .finish(),
        )
    }

    pub(super) fn output(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    pub(super) fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

pub(super) fn operation_id(line: &str) -> &str {
    let id = line.split_once("operation_id=").expect("operation ID").1;
    id.split(|c: char| !c.is_ascii_digit()).next().unwrap()
}

pub(super) fn summary(output: &str) -> &str {
    let summaries: Vec<_> = output
        .lines()
        .filter(|line| line.contains("Sync route outcome"))
        .collect();
    assert_eq!(summaries.len(), 1, "one summary per operation: {output}");
    summaries[0]
}

pub(super) fn assert_fields(line: &str, fields: &[&str]) {
    for field in fields {
        assert!(line.contains(field), "missing {field}: {line}");
    }
}

#[derive(Clone, Copy)]
enum Reply {
    Normal,
    Silent,
    SilentHandshake,
    Refused,
}

struct BridgeHandler {
    inner: SyncHandlerImpl,
    reply: Reply,
    handshake_delay: Duration,
}

#[async_trait]
impl SyncHandler for BridgeHandler {
    async fn handle_request(
        &self,
        request: &SyncRequest,
        context: &RequestContext,
    ) -> SyncResponse {
        match request {
            SyncRequest::Handshake(_) => {
                let delay = if matches!(self.reply, Reply::SilentHandshake) {
                    Duration::from_secs(31)
                } else {
                    self.handshake_delay
                };
                tokio::time::sleep(delay).await;
            }
            SyncRequest::SyncTree(_) => match self.reply {
                Reply::Normal | Reply::SilentHandshake => {}
                // Long enough to hit the real HTTP read timeout, but bounded so
                // the test leaves no permanently pending handler behind.
                Reply::Silent => tokio::time::sleep(Duration::from_secs(31)).await,
                Reply::Refused => {
                    return SyncResponse::BootstrapRejected {
                        request_id: "refusal-fixture".to_string(),
                        message: "access refused".to_string(),
                    };
                }
            },
            _ => {}
        }
        self.inner.handle_request(request, context).await
    }
}

struct Bridge {
    instance: Instance,
    tree: Database,
    transport: HttpTransport,
    address: Address,
}

impl Bridge {
    async fn start(reply: Reply, handshake_delay: Duration) -> Result<Self> {
        let (instance, mut user) = Instance::create_backend(
            Box::new(InMemory::new()),
            NewAccount::passwordless("bridge"),
        )
        .await?;
        let key = user.get_default_key()?;
        let tree = user.create_database(Doc::new(), &key).await?;
        let tx = tree.new_transaction().await?;
        tx.get_settings()?
            .set_global_auth_key(AuthKey::active(None, Permission::Read))
            .await?;
        tx.commit().await?;
        user.track_database(tree.root_id().clone(), &key, SyncSettings::enabled())
            .await?;
        let sync = Sync::new(instance.clone()).await?;
        sync.sync_account(user.login_id(), user.account().id())
            .await?;
        let transport = HttpTransport::builder().bind("127.0.0.1:0").build_sync()?;
        transport
            .start_server(Arc::new(BridgeHandler {
                inner: SyncHandlerImpl::new(instance.clone(), sync.sync_tree_root_id().clone()),
                reply,
                handshake_delay,
            }))
            .await?;
        let address = Address::http(transport.get_server_address()?);
        Ok(Self {
            instance,
            tree,
            transport,
            address,
        })
    }
}

async fn client(bridge: &Bridge, addresses: Vec<Address>) -> Result<(Instance, Sync)> {
    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::passwordless("daemon"),
    )
    .await?;
    let sync = Sync::new(instance.clone()).await?;
    sync.register_transport("http", HttpTransport::builder())
        .await?;
    sync.register_peer(&bridge.instance.id(), None).await?;
    for address in addresses {
        sync.add_peer_address(&bridge.instance.id(), address)
            .await?;
    }
    Ok((instance, sync))
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_pull_correlates_routes_engine_request_and_tree_lifecycle() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let bridge = Bridge::start(Reply::Normal, Duration::from_millis(100)).await?;
    let stranger = Bridge::start(Reply::Normal, Duration::ZERO).await?;
    let (instance, sync) = client(
        &bridge,
        vec![
            Address::new("unsupported", "route"),
            stranger.address.clone(),
            bridge.address.clone(),
        ],
    )
    .await?;
    trace.clear();
    sync.sync_tree_with_peer_auth(
        &bridge.instance.id(),
        bridge.tree.root_id(),
        Some(instance.signing_key()?),
        Some("daemon-key"),
        Some(Permission::Read),
        None,
    )
    .await?;
    let output = trace.output();
    let aggregate = summary(&output);
    assert_fields(
        aggregate,
        &[
            "outcome=\"success\"",
            "routes=3",
            "attempted=3",
            "route_failures=1",
            "wrong_peer=1",
            "unobserved=0",
            "unsupported=0",
            "trees_ok=1",
        ],
    );
    let id = operation_id(aggregate);
    for message in [
        "Address attempt failed",
        "Address answered as a different peer",
        "Selected sync route",
        "Starting tree synchronization",
        "Completed tree synchronization",
    ] {
        let event = output
            .lines()
            .find(|line| line.contains(message))
            .expect(message);
        assert_eq!(operation_id(event), id, "{event}");
    }
    // HTTP transport logs are generated in a separate engine-served task.
    let sent = output
        .lines()
        .find(|line| line.contains("sync_tree{") && line.contains("Sending sync request"))
        .expect("tree request kept the caller context across the engine channel");
    assert_eq!(operation_id(sent), id);
    assert!(instance.has_entry(bridge.tree.root_id()).await);
    bridge.transport.stop_server().await?;
    stranger.transport.stop_server().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_boundary_preserves_real_request_timeout_and_peer_rejection() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let silent = Bridge::start(Reply::Silent, Duration::ZERO).await?;
    let refusing = Bridge::start(Reply::Refused, Duration::ZERO).await?;
    let (_instance, sync) = client(&silent, vec![silent.address.clone()]).await?;
    sync.register_peer(&refusing.instance.id(), None).await?;
    sync.add_peer_address(&refusing.instance.id(), refusing.address.clone())
        .await?;
    trace.clear();
    let timed_out = sync
        .sync_tree_with_peer_auth(
            &silent.instance.id(),
            silent.tree.root_id(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&timed_out, Error::Sync(e) if matches!(**e, SyncError::Timeout { phase: TimeoutPhase::Request, .. }))
    );
    let output = trace.output();
    let aggregate = summary(&output);
    let timeout_id = operation_id(aggregate).to_string();
    assert_fields(
        aggregate,
        &[
            "outcome=\"failed\"",
            "timeout_phase=Some(Request)",
            "peer_reachable=Some(true)",
            "trees_failed=1",
        ],
    );
    assert!(!aggregate.contains("BootstrapRejected"));
    trace.clear();
    let refused = sync
        .sync_tree_with_peer_auth(
            &refusing.instance.id(),
            refusing.tree.root_id(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, Error::Sync(e) if matches!(**e, SyncError::BootstrapRejected { .. }))
    );
    let output = trace.output();
    let aggregate = summary(&output);
    assert_fields(
        aggregate,
        &[
            "outcome=\"failed\"",
            "BootstrapRejected",
            "timeout_phase=None",
        ],
    );
    assert_ne!(operation_id(aggregate), timeout_id);
    assert!(
        !output.contains("No address answered"),
        "the peer did answer"
    );
    silent.transport.stop_server().await?;
    refusing.transport.stop_server().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_ticket_operations_have_distinct_ids_and_one_summary_each() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let bridge = Bridge::start(Reply::Normal, Duration::ZERO).await?;
    let (_instance, sync) = client(&bridge, vec![bridge.address.clone()]).await?;
    let ticket =
        DatabaseTicket::with_addresses(bridge.tree.root_id().clone(), vec![bridge.address.clone()]);
    trace.clear();
    let (first, second) = tokio::join!(
        sync.sync_with_ticket(&ticket),
        sync.sync_with_ticket(&ticket)
    );
    first?;
    second?;
    let output = trace.output();
    let summaries: Vec<_> = output
        .lines()
        .filter(|line| line.contains("Sync route outcome"))
        .collect();
    assert_eq!(summaries.len(), 2, "{output}");
    assert_ne!(operation_id(summaries[0]), operation_id(summaries[1]));
    for line in summaries {
        assert_fields(
            line,
            &[
                "routes=1",
                "attempted=1",
                "unobserved=0",
                "outcome=\"success\"",
            ],
        );
    }
    bridge.transport.stop_server().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn all_failed_routes_and_empty_ticket_still_emit_one_summary() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let bridge = Bridge::start(Reply::Normal, Duration::ZERO).await?;
    let (_instance, sync) = client(&bridge, vec![Address::new("unsupported", "route")]).await?;
    trace.clear();
    assert!(
        sync.sync_tree_with_peer(&bridge.instance.id(), bridge.tree.root_id())
            .await
            .is_err()
    );
    let output = trace.output();
    assert_fields(
        summary(&output),
        &[
            "outcome=\"failed\"",
            "routes=1",
            "route_failures=1",
            "unobserved=0",
        ],
    );
    trace.clear();
    let empty = DatabaseTicket::with_addresses(bridge.tree.root_id().clone(), vec![]);
    assert!(sync.sync_with_ticket(&empty).await.is_err());
    let output = trace.output();
    assert_fields(
        summary(&output),
        &["outcome=\"failed\"", "routes=0", "attempted=0"],
    );
    bridge.transport.stop_server().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn failed_route_summary_counts_a_real_handshake_timeout() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let bridge = Bridge::start(Reply::SilentHandshake, Duration::ZERO).await?;
    let (_instance, sync) = client(
        &bridge,
        vec![bridge.address.clone(), Address::new("unsupported", "route")],
    )
    .await?;
    trace.clear();
    let error = sync
        .sync_tree_with_peer(&bridge.instance.id(), bridge.tree.root_id())
        .await
        .unwrap_err();
    // The race budget expires after TCP connected; it does not prove which
    // transport phase stalled and must not claim the peer was unreachable.
    assert!(matches!(error, Error::Sync(e) if matches!(*e, SyncError::Network(_))));
    let output = trace.output();
    let aggregate = summary(&output);
    assert_fields(
        aggregate,
        &[
            "outcome=\"failed\"",
            "routes=2",
            "attempted=2",
            "route_failures=2",
            "route_timeouts=1",
            "timeout_phase=None",
            "peer_reachable=None",
            "unobserved=0",
            "selected=None",
        ],
    );
    let timeout_event = output
        .lines()
        .find(|line| line.contains("Address attempt timed out"))
        .expect("deadline event");
    assert_eq!(operation_id(aggregate), operation_id(timeout_event));
    bridge.transport.stop_server().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn address_wrapper_reuses_one_operation_and_does_not_fail_losing_routes() -> Result<()> {
    let trace = Trace::default();
    let _guard = trace.capture();
    let bridge = Bridge::start(Reply::Normal, Duration::ZERO).await?;
    let silent = Bridge::start(Reply::SilentHandshake, Duration::ZERO).await?;
    let (_instance, sync) = client(
        &bridge,
        vec![bridge.address.clone(), silent.address.clone()],
    )
    .await?;
    trace.clear();

    sync.sync_with_peer(&bridge.address, Some(bridge.tree.root_id()))
        .await?;
    let output = trace.output();
    let aggregate = summary(&output);
    assert_fields(
        aggregate,
        &[
            "kind=\"sync_address\"",
            "outcome=\"success\"",
            "routes=3",
            "attempted=3",
            "route_failures=0",
            "route_timeouts=0",
            "unobserved=1",
            "trees_ok=1",
        ],
    );
    let id = operation_id(aggregate);
    let handshakes: Vec<_> = output
        .lines()
        .filter(|line| line.contains("Starting peer handshake"))
        .collect();
    assert!(
        handshakes.len() >= 2,
        "initial handshake and route selection: {output}"
    );
    for event in handshakes {
        assert_eq!(operation_id(event), id, "{event}");
    }
    let completed = output
        .lines()
        .find(|line| line.contains("Completed tree synchronization"))
        .expect("tree exchange completed");
    assert_eq!(operation_id(completed), id);
    bridge.transport.stop_server().await?;
    silent.transport.stop_server().await?;
    Ok(())
}

#[test]
fn operation_diagnostics_are_disabled_without_a_subscriber() {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        assert!(
            super::operation::SyncOperation::new("disabled")
                .span()
                .is_disabled()
        );
    });
}
