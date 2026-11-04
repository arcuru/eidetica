use std::time::Duration;

use crate::{
    auth::crypto::generate_keypair,
    backend::{BackendImpl, database::InMemory},
    sync::protocol::SyncTreeRequest,
};

use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn request(tree: &ID, known: Snapshot) -> SyncRequest {
    SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree.clone(),
        our_tips: known,
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    })
}

async fn fixture(payload: usize) -> (InMemory, Vec<Entry>) {
    let backend = InMemory::new();
    let root = Entry::root_builder()
        .set_metadata(b"framing fixture".to_vec())
        .build()
        .unwrap();
    let tree = root.id();
    let mut entries = vec![root];
    for n in 1..=18 {
        let entry = Entry::builder(tree.clone())
            .set_height(n)
            .add_parent(entries.last().unwrap().id())
            .set_subtree_data("data", "\"\\\n\t".repeat(payload))
            .build()
            .unwrap();
        entries.push(entry);
    }
    for entry in &entries {
        backend.put(entry.clone()).await.unwrap();
    }
    (backend, entries)
}

async fn sequence(
    backend: &InMemory,
    entries: &[Entry],
    known: Snapshot,
    count: usize,
    bytes: usize,
) -> (SyncRequest, Vec<Frame>) {
    let tree = entries[0].id();
    let request = request(&tree, known.clone());
    let start = HistoryStart {
        version: WIRE_VERSION,
        server: generate_keypair().1,
        request: request_binding(&request).unwrap(),
        tree: tree.clone(),
        target: Snapshot::from([entries.last().unwrap().id()]),
        kind: if known.is_empty() {
            HistoryKind::Bootstrap {
                key_approved: false,
                granted_permission: None,
            }
        } else {
            HistoryKind::Incremental
        },
        known,
    };
    let binding = start.binding().unwrap();
    let limits = start.page_limits(count, bytes).unwrap();
    let mut frames = vec![Frame::Start(start.clone())];
    let mut cursor = None;
    let mut total = 0;
    let mut pages = 0;
    loop {
        let page = backend
            .database_history_page(
                &tree,
                &start.target,
                &start.known,
                cursor.as_ref(),
                limits,
                deadline(),
            )
            .await
            .unwrap();
        total += page.entries.len() as u64;
        cursor = page.next.clone();
        if !page.entries.is_empty() {
            frames.push(Frame::Page {
                binding,
                ordinal: pages,
                page,
            });
            pages += 1;
        }
        if cursor.is_none() {
            break;
        }
    }
    frames.push(Frame::End {
        binding,
        pages,
        entries: total,
        covered: start.target,
    });
    (request, frames)
}

fn collect(request: SyncRequest, frames: &[Frame], chunk_size: usize) -> Result<SyncResponse> {
    let bytes: Vec<_> = frames.iter().flat_map(|f| encode(f).unwrap()).collect();
    let mut decoder = Decoder::default();
    let mut collector = Collector::new(request, deadline())?;
    for chunk in bytes.chunks(chunk_size) {
        receive_chunk(&mut decoder, &mut collector, chunk)?;
    }
    collector.finish(decoder)
}

#[tokio::test]
async fn peer_frames_large_native_history_roundtrips_across_page_and_chunk_sizes() {
    let (backend, entries) = fixture(12_000).await;
    assert!(serde_json::to_vec(&entries).unwrap().len() > MAX_FRAME_BYTES);
    for (count, bytes) in [(1, 160_000), (4, 300_000), (1024, JSON_CAP)] {
        let (request, frames) = sequence(&backend, &entries, Snapshot::EMPTY, count, bytes).await;
        assert!(frames.len() > 3);
        for frame in &frames {
            assert!(encode(frame).unwrap().len() <= MAX_FRAME_BYTES);
        }
        for chunk in [1, 8192, 2 * MAX_FRAME_BYTES] {
            let SyncResponse::Bootstrap(response) =
                collect(request.clone(), &frames, chunk).unwrap()
            else {
                panic!("not bootstrap");
            };
            let actual: Vec<_> = std::iter::once(response.root_entry)
                .chain(response.all_entries)
                .collect();
            assert_eq!(actual, entries);
        }
    }
}

#[tokio::test]
async fn peer_frames_incremental_boundary_and_empty_delta_are_complete() {
    let (backend, entries) = fixture(1).await;
    for known in [
        Snapshot::from([entries[8].id(), ID::from_bytes("unrelated known tip")]),
        Snapshot::from([entries.last().unwrap().id()]),
    ] {
        let empty = known.contains(&entries.last().unwrap().id());
        let (request, frames) = sequence(&backend, &entries, known, 2, 10_000).await;
        let SyncResponse::Incremental(response) = collect(request, &frames, 71).unwrap() else {
            panic!("not incremental");
        };
        assert_eq!(
            response.missing_entries,
            if empty { vec![] } else { entries[9..].to_vec() }
        );
        assert_eq!(response.their_tips, vec![entries.last().unwrap().id()]);
    }
}

#[tokio::test]
async fn peer_frames_reject_wrong_scope_binding_order_cursor_and_forged_completion() {
    let (backend, entries) = fixture(1).await;
    let (request, frames) = sequence(&backend, &entries, Snapshot::EMPTY, 2, 10_000).await;
    let rejects =
        |candidate: Vec<Frame>| assert!(collect(request.clone(), &candidate, 37).is_err());
    for change in 0..13 {
        let mut candidate = frames.clone();
        match change {
            0 => {
                if let Frame::Start(start) = &mut candidate[0] {
                    start.tree = ID::from_bytes("wrong scope");
                }
            }
            1 => {
                if let Frame::Start(start) = &mut candidate[0] {
                    start.request[0] ^= 1;
                }
            }
            2 => {
                if let Frame::Page { binding, .. } = &mut candidate[1] {
                    binding[0] ^= 1;
                }
            }
            3 => {
                if let Frame::Page { ordinal, .. } = &mut candidate[1] {
                    *ordinal = 1;
                }
            }
            4 => {
                if let Frame::Page { page, .. } = &mut candidate[1] {
                    page.entries.reverse();
                }
            }
            5 => {
                if let Frame::Page { page, .. } = &mut candidate[1] {
                    page.entries.clear();
                }
            }
            6 => {
                candidate.insert(2, candidate[1].clone());
            }
            7 => {
                candidate.remove(1);
            }
            8 => {
                if let Frame::End { entries, .. } = candidate.last_mut().unwrap() {
                    *entries += 1;
                }
            }
            9 => {
                if let Frame::End { covered, .. } = candidate.last_mut().unwrap() {
                    *covered = Snapshot::from([ID::from_bytes("forged target")]);
                }
            }
            10 => {
                candidate.pop();
            }
            11 => {
                if let Frame::Page { page, .. } = &mut candidate[1] {
                    let cursor = page.next.as_ref().unwrap();
                    let mut json = serde_json::to_value(cursor).unwrap();
                    json["height"] = serde_json::json!(99999);
                    page.next = Some(serde_json::from_value(json).unwrap());
                }
            }
            12 => {
                if let Frame::Start(start) = &mut candidate[0] {
                    start.version = 0;
                }
            }
            _ => unreachable!(),
        }
        rejects(candidate);
    }
    // Correct ordinals cannot hide a missing parent, even with forged counters.
    let mut missing = frames.clone();
    if let Frame::Page { page, .. } = &mut missing[1] {
        page.entries.remove(0);
    }
    if let Frame::End { entries, .. } = missing.last_mut().unwrap() {
        *entries -= 1;
    }
    rejects(missing);
    // A terminal page is not an End, and a terminal marker is not EOF.
    let mut trailing = frames.clone();
    trailing.push(Frame::Control(SyncResponse::Ack));
    rejects(trailing);
    let mut replay = frames.clone();
    replay.extend(frames.clone());
    rejects(replay);
    let mut error = frames;
    error.insert(2, Frame::Error(PeerError::Unsupported));
    rejects(error);
}

#[tokio::test]
async fn peer_frames_never_release_incomplete_cancelled_or_expired_prefix() {
    let (backend, entries) = fixture(1).await;
    let (request, frames) = sequence(&backend, &entries, Snapshot::EMPTY, 2, 10_000).await;
    for prefix in 0..frames.len() {
        assert!(collect(request.clone(), &frames[..prefix], 27).is_err());
    }
    let bytes: Vec<_> = frames.iter().flat_map(|f| encode(f).unwrap()).collect();
    for truncated in 1..=10 {
        let mut collector = Collector::new(request.clone(), deadline()).unwrap();
        let mut decoder = Decoder::default();
        receive_chunk(
            &mut decoder,
            &mut collector,
            &bytes[..bytes.len() - truncated],
        )
        .unwrap();
        assert!(collector.finish(decoder).is_err());
    }
    let mut collector = Collector::new(request.clone(), Instant::now()).unwrap();
    assert!(collector.push(frames[0].clone()).is_err());
    assert!(collector.finish(Decoder::default()).is_err());
    // A caller cannot catch a protocol failure and later obtain a valid result.
    let mut collector = Collector::new(request, deadline()).unwrap();
    assert!(collector.push(frames[1].clone()).is_err());
    assert!(collector.push(frames[0].clone()).is_err());
    assert!(collector.finish(Decoder::default()).is_err());
}

#[test]
fn peer_frames_decode_checks_length_before_allocating_and_poison_failures() {
    for size in [0u32, JSON_CAP as u32 + 1, u32::MAX] {
        let mut decoder = Decoder::default();
        assert!(decoder.next(&mut size.to_be_bytes().as_slice()).is_err());
        assert_eq!(decoder.body.capacity(), 0);
        assert!(
            decoder
                .next(
                    &mut encode(&Frame::Control(SyncResponse::Ack))
                        .unwrap()
                        .as_slice()
                )
                .is_err()
        );
        assert!(decoder.finish().is_err());
    }
    let mut decoder = Decoder::default();
    assert!(decoder.next(&mut b"\0\0\0\x01!".as_slice()).is_err());
    assert!(decoder.finish().is_err());
}

#[test]
fn peer_frames_complete_encoding_has_exact_boundary_with_json_escaping() {
    let mut frame = Frame::Control(SyncResponse::Error("\"\\\n".repeat(10)));
    let overhead = encode(&frame).unwrap().len();
    if let Frame::Control(SyncResponse::Error(message)) = &mut frame {
        message.push_str(&"x".repeat(MAX_FRAME_BYTES - overhead));
    }
    assert_eq!(encode(&frame).unwrap().len(), MAX_FRAME_BYTES);
    if let Frame::Control(SyncResponse::Error(message)) = &mut frame {
        message.push('x');
    }
    assert!(encode(&frame).is_err());
}

#[test]
fn peer_frames_typed_oversize_and_unframed_entry_controls_fail_closed() {
    let tree = ID::from_bytes("authorized entry");
    let request = request(&tree, Snapshot::EMPTY);
    let mut collector = Collector::new(request, deadline()).unwrap();
    let error = collector
        .push(Frame::Error(PeerError::EntryTooLarge {
            id: tree.clone(),
            limit: MAX_FRAME_BYTES,
            encoded_bytes: MAX_FRAME_BYTES + 1,
        }))
        .unwrap_err();
    assert!(
        matches!(error, crate::Error::Sync(e) if matches!(&*e, SyncError::EntryTooLarge { id, .. } if *id == tree))
    );
    let mut collector =
        Collector::new(super::tests::request(&tree, Snapshot::EMPTY), deadline()).unwrap();
    assert!(
        collector
            .push(Frame::Control(SyncResponse::Incremental(
                IncrementalResponse {
                    tree_id: tree,
                    their_tips: vec![],
                    missing_entries: vec![],
                }
            )))
            .is_err()
    );
}

#[tokio::test]
async fn peer_frames_uploads_page_borrowed_inputs_preserve_duplicates_and_database_order() {
    let (_, entries) = fixture(12_000).await;
    let (_, other) = fixture(1).await;
    // Both fixtures share a deterministic root, so use a distinct root too.
    let foreign = Entry::root_builder()
        .set_metadata(b"other database".to_vec())
        .build()
        .unwrap();
    let mut input = entries.clone();
    input.extend(other);
    input.push(entries[3].clone());
    input.push(foreign);
    let request = SyncRequest::SendEntries(input.clone());
    let mut bytes = Vec::new();
    write_request(&mut bytes, &request).await.unwrap();
    assert!(bytes.len() > MAX_FRAME_BYTES);
    let mut decoder = Decoder::default();
    let mut collector = RequestCollector::new(deadline());
    let mut inspect = bytes.as_slice();
    let mut frames = Vec::new();
    while !inspect.is_empty() {
        if let Some(frame) = decoder.next(&mut inspect).unwrap() {
            frames.push(frame);
        }
    }
    decoder.finish().unwrap();
    assert!(
        frames
            .iter()
            .filter(|f| matches!(f, Frame::UploadPage { .. }))
            .count()
            > 2
    );
    assert!(
        frames
            .iter()
            .all(|f| encode(f).unwrap().len() <= MAX_FRAME_BYTES)
    );
    let mut decoder = Decoder::default();
    for chunk in bytes.chunks(17) {
        receive_request_chunk(&mut decoder, &mut collector, chunk).unwrap();
    }
    let SyncRequest::SendEntries(actual) = collector.finish(decoder).unwrap() else {
        panic!("not upload");
    };
    input.sort_by_key(|entry| entry.root().unwrap_or_else(|| entry.id()));
    assert_eq!(actual, input);
    // Every cut while a Database submission is unfinished fails, including a
    // correct last-page offset without its required final receipt boundary.
    for n in 1..frames.len() {
        if matches!(frames[n - 1], Frame::UploadEnd { .. }) {
            continue;
        }
        let mut collector = RequestCollector::new(deadline());
        for frame in &frames[..n] {
            collector.push(frame.clone()).unwrap();
        }
        assert!(collector.finish(Decoder::default()).is_err());
    }
    for bad in 0..4 {
        let mut altered = frames.clone();
        let index = altered
            .iter()
            .position(|f| matches!(f, Frame::UploadPage { next: Some(_), .. }))
            .unwrap();
        if let Frame::UploadPage {
            offset,
            binding,
            next,
            entries,
        } = &mut altered[index]
        {
            match bad {
                0 => *offset += 1,
                1 => binding[0] ^= 1,
                2 => *next = None,
                _ => entries.clear(),
            }
        }
        let mut collector = RequestCollector::new(deadline());
        assert!(
            altered
                .into_iter()
                .try_for_each(|f| collector.push(f))
                .is_err()
        );
        assert!(collector.finish(Decoder::default()).is_err());
    }
}

#[tokio::test]
async fn peer_frames_singleton_upload_refusal_and_request_truncation_are_explicit() {
    let entry = Entry::root_builder()
        .set_subtree_data("huge", vec![255u8; MAX_FRAME_BYTES])
        .build()
        .unwrap();
    let id = entry.id();
    let mut bytes = Vec::new();
    let error = write_request(&mut bytes, &SyncRequest::SendEntries(vec![entry]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, crate::Error::Sync(e) if matches!(&*e, SyncError::EntryTooLarge { id: actual, .. } if *actual == id))
    );
    let mut decoder = Decoder::default();
    let mut collector = RequestCollector::new(deadline());
    receive_request_chunk(&mut decoder, &mut collector, &bytes).unwrap();
    assert!(collector.finish(decoder).is_err());
    let mut collector = RequestCollector::new(Instant::now());
    assert!(
        collector
            .push(Frame::Request(SyncRequest::SendEntries(Vec::new())))
            .is_err()
    );
}
