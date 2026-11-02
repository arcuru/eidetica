//! Real FIFO socket tests for strict v2 collection and bounded downgrades.
use super::*;
use crate::backend::{HistoryDelta, history_delta::DeltaQuery};
use crate::service::protocol::{FEATURE_HISTORY_DELTA, FEATURE_HISTORY_PAGES};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
fn fixtures() -> (ID, Snapshot, Vec<Entry>) {
    let root = Entry::root_builder()
        .set_subtree_data("s", "opaque ciphertext")
        .build()
        .unwrap();
    let child = Entry::builder(root.id())
        .add_parent(root.id())
        .set_subtree_data("s", "ciphertext")
        .set_subtree_height("s", Some(1))
        .add_subtree_parent("s", root.id())
        .build()
        .unwrap();
    (root.id(), Snapshot::from([child.id()]), vec![root, child])
}
async fn delta_peer() -> (RemoteConnection, UnixStream) {
    super::tests::connect_peer_features(vec![
        FEATURE_HISTORY_DELTA.into(),
        FEATURE_HISTORY_PAGES.into(),
    ])
    .await
}
async fn request(peer: &mut UnixStream) -> ServiceRequest {
    tokio::time::timeout(Duration::from_secs(5), read_frame(peer))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn response(peer: &mut UnixStream, response: ServiceResponse) {
    write_frame(peer, &ServerFrame::Response(Box::new(response)))
        .await
        .unwrap();
}
fn page(tree: &ID, target: &Snapshot, entries: &[Entry], last: bool) -> HistoryDelta {
    HistoryDelta {
        entries: entries.to_vec(),
        covered: Snapshot::from([entries.last().unwrap().id()]),
        binding: DeltaQuery::new(tree, "s", target, &Snapshot::EMPTY)
            .unwrap()
            .binding,
        complete: last,
    }
}
fn is_delta(req: ServiceRequest, target: &Snapshot, known: &Snapshot) {
    let ServiceRequest::AuthenticatedDb(req) = req else {
        panic!()
    };
    let DatabaseOp::GetStoreHistoryDelta {
        target: t,
        known: k,
        scope,
        ..
    } = req.op
    else {
        panic!("expected v2 delta")
    };
    assert_eq!((&t, &k), (target, known));
    assert!(matches!(scope, ReadScope::AllowUnverified));
}
#[tokio::test]
async fn history_delta_collector_fifo_notifications_and_complete_result() {
    let (conn, mut peer) = delta_peer().await;
    let (tree, target, entries) = fixtures();
    let client = async {
        assert_eq!(
            conn.get_store_entries(
                tree.clone(),
                SigKey::default(),
                "s".into(),
                target.tips().to_vec(),
                ReadScope::AllowUnverified
            )
            .await
            .unwrap(),
            entries
        );
        assert!(conn.get_instance_metadata().await.unwrap().is_none());
    };
    let server = async {
        is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
        write_frame(
            &mut peer,
            &serde_json::json!({"Notification":{"FutureOptional":{}}}),
        )
        .await
        .unwrap();
        response(
            &mut peer,
            ServiceResponse::HistoryDelta(page(&tree, &target, &entries[..1], false)),
        )
        .await;
        is_delta(
            request(&mut peer).await,
            &target,
            &Snapshot::from([entries[0].id()]),
        );
        response(
            &mut peer,
            ServiceResponse::HistoryDelta(page(&tree, &target, &entries[1..], true)),
        )
        .await;
        assert!(matches!(
            request(&mut peer).await,
            ServiceRequest::GetInstanceMetadata
        ));
        response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn history_delta_collector_invalid_progress_never_falls_back_or_returns_prefix() {
    // First-page errors also never disable/downgrade. After-progress Unsupported
    // cannot restart with v1, even though this peer advertises both versions.
    for mode in 0..12 {
        let (conn, mut peer) = delta_peer().await;
        let (tree, target, entries) = fixtures();
        let call = conn.get_store_entries(
            tree.clone(),
            SigKey::default(),
            "s".into(),
            target.tips().to_vec(),
            ReadScope::AllowUnverified,
        );
        let server = async {
            is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
            if mode >= 6 {
                response(
                    &mut peer,
                    ServiceResponse::HistoryDelta(page(&tree, &target, &entries[..1], false)),
                )
                .await;
                is_delta(
                    request(&mut peer).await,
                    &target,
                    &Snapshot::from([entries[0].id()]),
                );
            }
            let mut p = page(
                &tree,
                &target,
                if mode >= 6 { &entries[1..] } else { &entries },
                true,
            );
            match mode % 6 {
                0 => p.binding[0] ^= 1,
                1 => p.covered = Snapshot::EMPTY,
                2 => p.complete = false,
                3 => {
                    p.entries.clear();
                    p.complete = false;
                }
                4 => {
                    p.entries.reverse();
                    if mode >= 6 {
                        p.entries = vec![entries[0].clone()];
                    }
                }
                _ => {
                    response(
                        &mut peer,
                        if mode >= 6 {
                            ServiceResponse::Unsupported
                        } else {
                            ServiceResponse::Error(crate::service::error::ServiceError {
                                module: "backend".into(),
                                kind: "HistoryResourceLimit".into(),
                                message: "finite workspace".into(),
                            })
                        },
                    )
                    .await;
                    return;
                }
            }
            response(&mut peer, ServiceResponse::HistoryDelta(p)).await;
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(call, server) })
                .await
                .unwrap();
        assert!(result.is_err(), "mode {mode} returned partial success");
        assert!(
            conn.has_feature(FEATURE_HISTORY_DELTA),
            "mode {mode} disabled v2"
        );
        let mut probe = Box::pin(conn.get_instance_metadata());
        assert!(futures_util::poll!(probe.as_mut()).is_pending());
        assert!(
            matches!(
                request(&mut peer).await,
                ServiceRequest::GetInstanceMetadata
            ),
            "invalid reply caused fallback"
        );
        response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
        assert!(probe.await.unwrap().is_none());
    }
}
#[tokio::test]
async fn history_delta_preprogress_chain_disables_once_and_arbitrary_known_never_substitutes() {
    let (conn, mut peer) = delta_peer().await;
    let (tree, target, entries) = fixtures();
    let client = async {
        for _ in 0..2 {
            assert_eq!(
                conn.get_store_entries(
                    tree.clone(),
                    SigKey::default(),
                    "s".into(),
                    target.tips().to_vec(),
                    ReadScope::AllowUnverified
                )
                .await
                .unwrap(),
                entries
            );
        }
        assert!(!conn.has_feature(FEATURE_HISTORY_DELTA));
        assert!(!conn.has_feature(FEATURE_HISTORY_PAGES));
    };
    let server = async {
        is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
        response(&mut peer, ServiceResponse::Unsupported).await;
        let ServiceRequest::AuthenticatedDb(req) = request(&mut peer).await else {
            panic!()
        };
        assert!(matches!(req.op, DatabaseOp::GetStoreHistoryPage { .. }));
        response(&mut peer, ServiceResponse::Unsupported).await;
        for _ in 0..2 {
            let ServiceRequest::AuthenticatedDb(req) = request(&mut peer).await else {
                panic!()
            };
            assert!(matches!(req.op, DatabaseOp::GetStoreEntries { .. }));
            response(&mut peer, ServiceResponse::Entries(entries.clone())).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    let (conn, mut peer) = delta_peer().await;
    let known = Snapshot::from([entries[0].id()]);
    let call = conn.get_store_history_delta(
        tree.clone(),
        SigKey::default(),
        "s".into(),
        target.clone(),
        known.clone(),
        ReadScope::AllowUnverified,
        crate::backend::HistoryPageLimits {
            max_entries: 1,
            max_encoded_bytes: 4096,
            allow_large_singleton: false,
        },
    );
    let server = async {
        is_delta(request(&mut peer).await, &target, &known);
        response(&mut peer, ServiceResponse::Unsupported).await;
    };
    let (result, ()) = tokio::join!(call, server);
    assert!(result.is_err());
    assert!(conn.has_feature(FEATURE_HISTORY_DELTA));
}
#[tokio::test]
async fn history_delta_cancel_and_eof_keep_fifo_no_replay() {
    for eof in [false, true] {
        let (conn, mut peer) = delta_peer().await;
        let (tree, target, entries) = fixtures();
        let mut call = Box::pin(conn.get_store_entries(
            tree.clone(),
            SigKey::default(),
            "s".into(),
            target.tips().to_vec(),
            ReadScope::AllowUnverified,
        ));
        assert!(futures_util::poll!(call.as_mut()).is_pending());
        is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
        response(
            &mut peer,
            ServiceResponse::HistoryDelta(page(&tree, &target, &entries[..1], false)),
        )
        .await;
        if eof {
            let reader = async {
                is_delta(
                    request(&mut peer).await,
                    &target,
                    &Snapshot::from([entries[0].id()]),
                );
                peer.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(call, reader);
            assert!(result.is_err());
            assert!(conn.get_instance_metadata().await.is_err());
        } else {
            drop(call);
            let mut next = Box::pin(conn.get_instance_metadata());
            assert!(futures_util::poll!(next.as_mut()).is_pending());
            assert!(matches!(
                request(&mut peer).await,
                ServiceRequest::GetInstanceMetadata
            ));
            response(&mut peer, ServiceResponse::InstanceMetadata(None)).await;
            assert!(next.await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn history_delta_one_page_enforces_complete_encoded_response_limit() {
    let (conn, mut peer) = delta_peer().await;
    let (tree, target, entries) = fixtures();
    let call = conn.get_store_history_delta(
        tree.clone(),
        SigKey::default(),
        "s".into(),
        target.clone(),
        Snapshot::EMPTY,
        ReadScope::AllowUnverified,
        crate::backend::HistoryPageLimits {
            max_entries: 2,
            max_encoded_bytes: 16,
            allow_large_singleton: false,
        },
    );
    let server = async {
        is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
        response(
            &mut peer,
            ServiceResponse::HistoryDelta(page(&tree, &target, &entries, true)),
        )
        .await;
    };
    let (result, ()) = tokio::join!(call, server);
    assert!(result.is_err());
    assert!(conn.has_feature(FEATURE_HISTORY_DELTA));
}

#[tokio::test]
async fn history_delta_collector_terminal_frontier_is_target_local() {
    for empty in [false, true] {
        let (conn, mut peer) = delta_peer().await;
        let (tree, original, entries) = fixtures();
        let target = if empty { Snapshot::EMPTY } else { original };
        let call = conn.get_store_entries(
            tree.clone(),
            SigKey::default(),
            "s".into(),
            target.tips().to_vec(),
            ReadScope::AllowUnverified,
        );
        let server = async {
            is_delta(request(&mut peer).await, &target, &Snapshot::EMPTY);
            let mut p = page(&tree, &target, &entries, true);
            if !empty {
                let extra = Entry::builder(tree.clone())
                    .add_parent(tree.clone())
                    .set_subtree_data("s", "outside target")
                    .set_subtree_height("s", Some(2))
                    .add_subtree_parent("s", entries[0].id())
                    .build()
                    .unwrap();
                p.covered = Snapshot::from([entries[1].id(), extra.id()]);
                p.entries.push(extra);
            }
            response(&mut peer, ServiceResponse::HistoryDelta(p)).await;
        };
        let (result, ()) = tokio::join!(call, server);
        assert!(
            result.is_err(),
            "accepted extraneous terminal branch for empty={empty}"
        );
    }
}
