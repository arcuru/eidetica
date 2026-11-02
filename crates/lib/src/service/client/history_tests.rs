//! Collector tests use the real socket/FIFO reader and ordinary public method.
use super::*;
use crate::backend::{HistoryPage, history_page::HistoryQuery};
use crate::service::protocol::{FEATURE_HISTORY_PAGES, MAX_FRAME_SIZE};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

async fn peer() -> (RemoteConnection, UnixStream) {
    super::tests::connect_peer_features(vec![FEATURE_HISTORY_PAGES.into()]).await
}
fn fixtures() -> (ID, Vec<ID>, Vec<Entry>) {
    let root = Entry::root_builder()
        .set_subtree_data("s", "opaque encrypted bytes")
        .build()
        .unwrap();
    let child = Entry::builder(root.id())
        .add_parent(root.id())
        .set_subtree_data("s", "ciphertext")
        .add_subtree_parent("s", root.id())
        .set_subtree_height("s", Some(1))
        .build()
        .unwrap();
    (root.id(), vec![child.id()], vec![root, child])
}
fn first(tree: &ID, tips: &[ID], entries: &[Entry]) -> HistoryPage {
    let snapshot = Snapshot::from(tips.to_vec());
    let q = HistoryQuery::new(tree, "s", &snapshot, None).unwrap();
    HistoryPage {
        entries: vec![entries[0].clone()],
        next: Some(q.cursor(0, entries[0].id_ref())),
    }
}
async fn response(peer: &mut UnixStream, response: ServiceResponse) {
    write_frame(peer, &ServerFrame::Response(Box::new(response)))
        .await
        .unwrap();
}
async fn request(peer: &mut UnixStream) -> ServiceRequest {
    tokio::time::timeout(Duration::from_secs(3), read_frame(peer))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
fn is_page(req: ServiceRequest, tree: &ID, tips: &[ID], has_cursor: bool) {
    let ServiceRequest::AuthenticatedDb(req) = req else {
        panic!("not authenticated DB request")
    };
    assert_eq!(req.root_id, *tree);
    assert_eq!(req.identity, SigKey::default());
    let DatabaseOp::GetStoreHistoryPage {
        store,
        tips: actual,
        scope,
        after,
        max_entries,
        max_encoded_bytes,
        allow_large_singleton,
    } = req.op
    else {
        panic!("not a page request")
    };
    assert_eq!(store, "s");
    assert_eq!(actual, tips);
    assert!(matches!(scope, ReadScope::AllowUnverified));
    assert_eq!(after.is_some(), has_cursor);
    assert_eq!(max_entries, 1024);
    assert_eq!(max_encoded_bytes, 4 * 1024 * 1024);
    assert!(allow_large_singleton);
}

#[tokio::test]
async fn history_collector_context_concurrent_rpc_notifications_and_complete_result() {
    let (conn, mut peer) = peer().await;
    let (tree, tips, entries) = fixtures();
    let mut call = Box::pin(conn.get_store_entries(
        tree.clone(),
        SigKey::default(),
        "s".into(),
        tips.clone(),
        ReadScope::AllowUnverified,
    ));
    assert!(futures_util::poll!(call.as_mut()).is_pending());
    is_page(request(&mut peer).await, &tree, &tips, false);
    let mut metadata = Box::pin(conn.get_instance_metadata());
    assert!(futures_util::poll!(metadata.as_mut()).is_pending());
    assert!(matches!(
        request(&mut peer).await,
        ServiceRequest::GetInstanceMetadata
    ));
    // Unknown optional notifications consume no response slot, just as actual
    // cursor notifications have an independent demultiplexer.
    write_frame(
        &mut peer,
        &serde_json::json!({"Notification":{"FutureOptional":{}}}),
    )
    .await
    .unwrap();
    write_frame(
        &mut peer,
        &ServerFrame::Notification(Notification::DatabaseWrite {
            root_id: tree.clone(),
            previous_tips: Snapshot::EMPTY,
            post_tips: Snapshot::from(tips.clone()),
            source: crate::instance::WriteSource::Local,
        }),
    )
    .await
    .unwrap();
    response(
        &mut peer,
        ServiceResponse::HistoryPage(first(&tree, &tips, &entries)),
    )
    .await;
    response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
    let client = async {
        let got = call.await.unwrap();
        assert_eq!(got, entries);
        assert!(metadata.await.unwrap().is_none());
    };
    let server = async {
        is_page(request(&mut peer).await, &tree, &tips, true);
        response(
            &mut peer,
            ServiceResponse::HistoryPage(HistoryPage {
                entries: vec![entries[1].clone()],
                next: None,
            }),
        )
        .await;
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn history_collector_rejects_partial_eof_errors_order_and_nonprogress_without_fallback() {
    for mode in 0..8 {
        let (conn, mut peer) = peer().await;
        let (tree, tips, entries) = fixtures();
        let call = conn.get_store_entries(
            tree.clone(),
            SigKey::default(),
            "s".into(),
            tips.clone(),
            ReadScope::AllowUnverified,
        );
        let server = async {
            is_page(request(&mut peer).await, &tree, &tips, false);
            response(
                &mut peer,
                ServiceResponse::HistoryPage(first(&tree, &tips, &entries)),
            )
            .await;
            is_page(request(&mut peer).await, &tree, &tips, true);
            match mode {
                0 => response(&mut peer, ServiceResponse::Unsupported).await,
                1 => {
                    response(
                        &mut peer,
                        ServiceResponse::HistoryPage(first(&tree, &tips, &entries)),
                    )
                    .await
                }
                2 => {
                    response(
                        &mut peer,
                        ServiceResponse::HistoryPage(HistoryPage {
                            entries: vec![entries[0].clone()],
                            next: None,
                        }),
                    )
                    .await
                }
                3 => {
                    response(
                        &mut peer,
                        ServiceResponse::HistoryPage(HistoryPage {
                            entries: vec![],
                            next: first(&tree, &tips, &entries).next,
                        }),
                    )
                    .await
                }
                4 => {
                    peer.write_all(&[0, 0]).await.unwrap();
                    peer.shutdown().await.unwrap();
                }
                5 => {
                    peer.write_all(&100u32.to_be_bytes()).await.unwrap();
                    peer.write_all(b"{").await.unwrap();
                    peer.shutdown().await.unwrap();
                }
                6 => {
                    peer.shutdown().await.unwrap();
                }
                _ => {
                    response(
                        &mut peer,
                        ServiceResponse::Error(crate::service::error::ServiceError::from(
                            &crate::backend::BackendError::HistoryResourceLimit {
                                resource: "fixture".into(),
                                limit: 16,
                            }
                            .into(),
                        )),
                    )
                    .await
                }
            }
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(call, server) })
                .await
                .unwrap();
        assert!(result.is_err(), "mode {mode} returned a successful prefix");
        if mode == 0 {
            assert!(
                conn.has_feature(FEATURE_HISTORY_PAGES),
                "Unsupported after progress disabled the feature"
            );
        }
        // No generic fallback can have reached the peer after the failed page.
        if !conn.inner.closed.load(Ordering::Acquire) {
            let mut probe = Box::pin(conn.get_instance_metadata());
            assert!(futures_util::poll!(probe.as_mut()).is_pending());
            assert!(matches!(
                request(&mut peer).await,
                ServiceRequest::GetInstanceMetadata
            ));
            response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
            assert!(probe.await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn history_collector_unsupported_before_progress_disables_once_only() {
    let (conn, mut peer) = peer().await;
    let (tree, tips, entries) = fixtures();
    let client = async {
        for _ in 0..2 {
            assert_eq!(
                conn.get_store_entries(
                    tree.clone(),
                    SigKey::default(),
                    "s".into(),
                    tips.clone(),
                    ReadScope::AllowUnverified
                )
                .await
                .unwrap(),
                entries
            );
        }
        assert!(!conn.has_feature(FEATURE_HISTORY_PAGES));
    };
    let server = async {
        is_page(request(&mut peer).await, &tree, &tips, false);
        response(&mut peer, ServiceResponse::Unsupported).await;
        for _ in 0..2 {
            let ServiceRequest::AuthenticatedDb(req) = request(&mut peer).await else {
                panic!()
            };
            assert!(matches!(req.op, DatabaseOp::GetStoreEntries { .. }));
            response(&mut peer, ServiceResponse::Entries(entries.clone())).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn history_frame_exact_cap_minus_one_exact_and_plus_one() {
    // A JSON string's two quotation marks are part of the physical payload.
    for size in [MAX_FRAME_SIZE - 1, MAX_FRAME_SIZE, MAX_FRAME_SIZE + 1] {
        let value = "x".repeat(size as usize - 2);
        let mut bytes = Vec::new();
        let result = write_frame(&mut bytes, &value).await;
        assert_eq!(result.is_ok(), size <= MAX_FRAME_SIZE);
        if result.is_err() {
            assert!(bytes.is_empty(), "local overflow wrote a prefix");
        } else {
            assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), size);
        }
    }
}

#[tokio::test]
async fn history_collector_cancel_at_page_boundary_leaves_fifo_slot_to_drain() {
    let (conn, mut peer) = peer().await;
    let (tree, tips, entries) = fixtures();
    let mut call = Box::pin(conn.get_store_entries(
        tree.clone(),
        SigKey::default(),
        "s".into(),
        tips.clone(),
        ReadScope::AllowUnverified,
    ));
    assert!(futures_util::poll!(call.as_mut()).is_pending());
    is_page(request(&mut peer).await, &tree, &tips, false);
    response(
        &mut peer,
        ServiceResponse::HistoryPage(first(&tree, &tips, &entries)),
    )
    .await;
    drop(call);
    let mut later = Box::pin(conn.get_instance_metadata());
    assert!(futures_util::poll!(later.as_mut()).is_pending());
    assert!(
        matches!(
            request(&mut peer).await,
            ServiceRequest::GetInstanceMetadata
        ),
        "cancelled collector sent another page"
    );
    response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), later)
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
}
