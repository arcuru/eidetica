//! Codec, bridge and progress behavior through the production message paths.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use serde_json::json;
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

use super::{
    ProgressMonitor, ReadOptions, ResourceLimit, TransferBudget, TransferError, TransferPolicy,
    TransferUsage,
    codec::{CHUNK_BYTES, INLINE_MESSAGE_BYTES, decode, encode_inline},
    prepare_message, read_message,
};
use crate::{
    Entry,
    auth::{crypto::PrivateKey, types::AuthInfo},
    sync::protocol::{SyncRequest, SyncResponse},
};

#[cfg(all(unix, feature = "service"))]
use crate::{
    account::{AccountCredentials, AccountInfo, AccountStatus, KeyStorage},
    entry::ID,
    service::protocol::{
        IncomingRequest, IncomingServerFrame, ServerFrame, ServiceResponse, TransactionContext,
        WireRecordPage, encode_json_fixture,
    },
};

const MIB: usize = 1024 * 1024;

#[test]
fn truncated_enum_map_must_not_acknowledge() {
    let valid = encode(&SyncResponse::Ack);
    assert!(matches!(
        decode::<SyncResponse, _>(valid.as_slice(), &policy()),
        Ok(SyncResponse::Ack)
    ));
    let malformed = [0xa1, 0x63, b'A', b'c', b'k'];
    let result = decode::<SyncResponse, _>(malformed.as_slice(), &policy());
    assert!(
        result.is_err(),
        "map missing its value accepted: {result:?}"
    );
}

#[test]
fn enums_require_complete_single_entry_maps_and_unit_only_strings() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Example {
        Unit,
        Newtype(u8),
        Tuple(u8, u8),
        Struct { value: u8 },
    }
    for value in [
        Example::Unit,
        Example::Newtype(1),
        Example::Tuple(2, 3),
        Example::Struct { value: 4 },
    ] {
        let encoded = encode(&vec![value]);
        let decoded: Vec<Example> = decode(encoded.as_slice(), &policy()).unwrap();
        assert_eq!(encode(&decoded), encoded);
        // Includes truncated unit/newtype/tuple/struct payloads nested in a sequence.
        assert!(decode::<Vec<Example>, _>(&encoded[..encoded.len() - 1], &policy()).is_err());
    }
    assert_eq!(
        decode::<Example, _>(fixture(r#""Unit""#).as_slice(), &policy()).unwrap(),
        Example::Unit
    );
    // Map-form units must consume the native unit encoding too.
    assert_eq!(
        decode::<Example, _>(
            [0xa1, 0x64, b'U', b'n', b'i', b't', 0x80].as_slice(),
            &policy()
        )
        .unwrap(),
        Example::Unit
    );
    for malformed in [
        r#""Newtype""#,
        r#""Tuple""#,
        r#""Struct""#,
        r#"{}"#,
        r#"{"Unit":1}"#,
        r#"{"Newtype":1,"Other":2}"#,
        r#"{"Newtype":1,"Newtype":2}"#,
    ] {
        assert!(
            decode::<Example, _>(fixture(malformed).as_slice(), &policy()).is_err(),
            "accepted {malformed}"
        );
    }
    let mut declared_extra = encode(&Example::Newtype(1));
    declared_extra[0] = 0xa2;
    assert!(decode::<Example, _>(declared_extra.as_slice(), &policy()).is_err());
    let limited = budget_with(|p| p.max_identifier_bytes = 2);
    assert!(matches!(
        decode::<Example, _>(encode(&Example::Newtype(1)).as_slice(), limited.policy()),
        Err(TransferError::Resource {
            limit: ResourceLimit::IdentifierBytes,
            ..
        })
    ));
    let limited = budget_with(|p| p.max_nodes = 1);
    assert!(matches!(
        decode::<Example, _>(encode(&Example::Newtype(1)).as_slice(), limited.policy()),
        Err(TransferError::Resource {
            limit: ResourceLimit::Nodes,
            ..
        })
    ));
}

fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    cbor4ii::serde::to_vec(Vec::new(), value).unwrap()
}

#[cfg(all(unix, feature = "service"))]
fn fixture(json: &str) -> Vec<u8> {
    encode_json_fixture(json).unwrap()
}

fn policy() -> TransferPolicy {
    TransferPolicy::default()
}

fn budget_with(change: impl FnOnce(&mut TransferPolicy)) -> TransferBudget {
    let mut policy = policy();
    change(&mut policy);
    TransferBudget::new(policy).unwrap()
}

fn refused<T>(result: Result<T, TransferError>) -> ResourceLimit {
    match result {
        Err(TransferError::Resource { limit, .. }) => limit,
        Err(other) => panic!("expected a resource refusal, got {other}"),
        Ok(_) => panic!("expected a resource refusal, got a value"),
    }
}

/// CBOR header of a byte string, so a test can stop partway through its content.
fn byte_string_header(len: usize) -> Vec<u8> {
    let mut header = vec![0x5a];
    header.extend_from_slice(&(len as u32).to_be_bytes());
    header
}

fn entry_with(bytes: usize) -> Entry {
    Entry::root_builder()
        .set_subtree_data("data", "x".repeat(bytes))
        .build()
        .unwrap()
}

/// Wall-clock wait, so it also works under a paused Tokio clock.
async fn wait_until_idle(budget: &TransferBudget) {
    let started = std::time::Instant::now();
    while budget.usage() != TransferUsage::default() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "admission must be returned once workers stop"
        );
        tokio::task::yield_now().await;
    }
}

async fn send<T: Serialize + Send + 'static>(
    writer: &mut DuplexStream,
    value: T,
    budget: &TransferBudget,
) -> Result<(), TransferError> {
    let monitor = ProgressMonitor::new(budget.policy());
    prepare_message(value, budget)
        .await?
        .write_to(writer, &monitor)
        .await
}

async fn receive<T: serde::de::DeserializeOwned + Send + 'static>(
    reader: &mut DuplexStream,
    budget: &TransferBudget,
) -> Result<T, TransferError> {
    let message = read_message::<_, T>(reader, budget, ReadOptions::default()).await?;
    Ok(message.expect("message").value)
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn json_null_stays_distinct_from_an_empty_array() {
    let value = json!({"nested": [null, [], {"null": null}], "text": "x"});
    let response = ServerFrame::Response(Box::new(ServiceResponse::CrdtValue(value.clone())));
    let decoded: IncomingServerFrame = decode(encode(&response).as_slice(), &policy()).unwrap();
    match decoded {
        IncomingServerFrame::Response(response) => match *response {
            ServiceResponse::CrdtValue(decoded) => assert_eq!(decoded, value),
            other => panic!("unexpected response {other:?}"),
        },
        IncomingServerFrame::Notification(_) => panic!("unexpected notification"),
    }

    let context = TransactionContext {
        main_parents: Vec::new(),
        subtree_parents: Default::default(),
        settings_tips: Vec::new(),
        settings_value: value.clone(),
    };
    let decoded: TransactionContext = decode(encode(&context).as_slice(), &policy()).unwrap();
    assert_eq!(decoded.settings_value, value);
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn both_key_storage_variants_cross_the_wire_unchanged() {
    let key = PrivateKey::generate();
    for root_key in [
        KeyStorage::Encrypted {
            algorithm: "aes-256-gcm".to_string(),
            ciphertext: vec![0xaa; 4096],
            nonce: vec![1; 12],
        },
        KeyStorage::Unencrypted { key: key.clone() },
    ] {
        let account_info = AccountInfo {
            username: "alice".to_string(),
            account_id: ID::from_bytes("account"),
            credentials: AccountCredentials {
                root_key_id: key.public_key(),
                root_key,
                password_salt: Some("salt".to_string()),
            },
            created_at: 7,
            status: AccountStatus::Active,
        };
        let response = ServiceResponse::TrustedLoginChallenge {
            challenge: vec![1, 2, 3],
            login_id: "login".to_string(),
            account_info: account_info.clone(),
        };
        let decoded: ServiceResponse = decode(encode(&response).as_slice(), &policy()).unwrap();
        let ServiceResponse::TrustedLoginChallenge {
            account_info: decoded,
            ..
        } = decoded
        else {
            panic!("unexpected response");
        };
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(account_info).unwrap()
        );
    }
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn a_record_is_one_value_however_long_it_is() {
    let page = crate::backend::RecordPage {
        records: vec![(vec![1; 8], vec![2; 2 * MIB])],
        next: Some(vec![3]),
    };
    let limited = budget_with(|policy| policy.max_nodes = 64);
    let wire = ServiceResponse::RecordPage(WireRecordPage::from(page.clone()));
    let decoded: ServiceResponse = decode(encode(&wire).as_slice(), limited.policy()).unwrap();
    let ServiceResponse::RecordPage(decoded) = decoded else {
        panic!("unexpected response");
    };
    assert_eq!(crate::backend::RecordPage::from(decoded), page);

    let record = ServiceResponse::Record(Some(vec![9; 2 * MIB]));
    let decoded: ServiceResponse = decode(encode(&record).as_slice(), limited.policy()).unwrap();
    assert!(matches!(decoded, ServiceResponse::Record(Some(value)) if value.len() == 2 * MIB));
}

#[test]
fn signed_entries_keep_their_identity_on_the_wire() {
    let key = PrivateKey::generate();
    let unsigned = entry_with(4096).with_auth(|auth| {
        *auth = AuthInfo::from_pubkey(&key.public_key());
    });
    let signature = crate::auth::crypto::sign_entry(&unsigned, &key).unwrap();
    let entry = unsigned.with_auth(|auth| auth.signature = Some(signature));

    let request = SyncRequest::SendEntries(vec![entry.clone()]);
    let decoded: SyncRequest = decode(encode(&request).as_slice(), &policy()).unwrap();
    let SyncRequest::SendEntries(decoded) = decoded else {
        panic!("unexpected request");
    };
    assert_eq!(decoded[0], entry);
    assert_eq!(decoded[0].id(), entry.id());
    crate::auth::crypto::verify_entry_signature(&decoded[0], &key.public_key()).unwrap();
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn optional_variants_classify_and_known_payloads_stay_strict() {
    let classify = |json: &str| decode::<IncomingRequest, _>(fixture(json).as_slice(), &policy());
    assert!(matches!(
        classify(r#""GetInstanceMetadata""#),
        Ok(IncomingRequest::Known(_))
    ));
    assert!(matches!(
        classify(r#"{"TrustedLoginUser":{"username":"a","extra":[1,{"b":null}]}}"#),
        Ok(IncomingRequest::Known(_))
    ));
    for unknown in [r#""Future""#, r#"{"Future":{"payload":[1,2,{"a":null}]}}"#] {
        assert!(matches!(
            classify(unknown),
            Ok(IncomingRequest::Unsupported)
        ));
    }
    for malformed in [
        "null",
        "[]",
        "{}",
        r#"{"Future":1,"Other":2}"#,
        r#"{"Future":1,"Future":2}"#,
        r#"{"TrustedLoginUser":{}}"#,
        r#"{"TrustedLoginUser":{"username":4}}"#,
        r#"{"TrustedLoginUser":{"username":"a","username":"b"}}"#,
        r#""TrustedLoginUser""#,
    ] {
        assert!(
            matches!(classify(malformed), Err(TransferError::Malformed(_))),
            "{malformed}"
        );
    }

    let frame = |json: &str| decode::<IncomingServerFrame, _>(fixture(json).as_slice(), &policy());
    assert!(matches!(
        frame(r#"{"Response":{"Future":[1]}}"#),
        Ok(IncomingServerFrame::Response(response))
            if matches!(*response, ServiceResponse::Unsupported)
    ));
    assert!(matches!(
        frame(r#"{"Notification":{"Future":[1]}}"#),
        Ok(IncomingServerFrame::Notification(None))
    ));
    assert!(frame(r#"{"Future":"Ok"}"#).is_err());
}

#[test]
fn truncation_and_trailing_bytes_are_never_a_value() {
    let bytes = encode(&SyncResponse::Count(7));
    for end in 0..bytes.len() {
        assert!(decode::<SyncResponse, _>(&bytes[..end], &policy()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        decode::<SyncResponse, _>(trailing.as_slice(), &policy()),
        Err(TransferError::Malformed(_))
    ));
}

#[test]
fn decode_quotas_refuse_by_name() {
    let nodes = encode(&vec![1u8; 100]);
    let limited = budget_with(|policy| policy.max_nodes = 99);
    assert_eq!(
        refused(decode::<Vec<u8>, _>(nodes.as_slice(), limited.policy())),
        ResourceLimit::Nodes
    );
    let enough = budget_with(|policy| policy.max_nodes = 100);
    assert_eq!(
        decode::<Vec<u8>, _>(nodes.as_slice(), enough.policy())
            .unwrap()
            .len(),
        100
    );

    let mut deep = serde_json::Value::Null;
    for _ in 0..8 {
        deep = json!([deep]);
    }
    let shallow = budget_with(|policy| policy.max_depth = 4);
    assert_eq!(
        refused(decode::<serde_json::Value, _>(
            encode(&deep).as_slice(),
            shallow.policy()
        )),
        ResourceLimit::Depth
    );
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn service_decode_byte_allowances_refuse_by_name() {
    #[derive(Deserialize, Debug)]
    struct Open {}

    // A discriminant is an identifier, whether a field name or a whole message.
    let long = "x".repeat(5000);
    for bytes in [
        fixture(&format!(r#""{long}""#)),
        fixture(&format!(r#"{{"{long}":1}}"#)),
    ] {
        assert_eq!(
            refused(decode::<IncomingRequest, _>(bytes.as_slice(), &policy())),
            ResourceLimit::IdentifierBytes
        );
    }
    let field = fixture(&format!(r#"{{"{long}":1}}"#));
    assert_eq!(
        refused(decode::<Open, _>(field.as_slice(), &policy())),
        ResourceLimit::IdentifierBytes
    );
    // Ordinary data keys are data, not identifiers.
    let data = fixture(&format!(r#"{{"{long}":1}}"#));
    let value: serde_json::Value = decode(data.as_slice(), &policy()).unwrap();
    assert_eq!(value[&long], 1);

    let large = format!(r#"{{"Future":"{}"}}"#, "x".repeat(70 * 1024));
    assert_eq!(
        refused(decode::<IncomingRequest, _>(
            fixture(&large).as_slice(),
            &policy()
        )),
        ResourceLimit::IgnoredBytes
    );
}

#[cfg(all(unix, feature = "service"))]
#[test]
fn discarded_values_are_validated_but_valid_tagged_ones_pass() {
    // {"Future": <text with an invalid UTF-8 byte>}
    let mut invalid = fixture(r#"{"Future":"x"}"#);
    *invalid.last_mut().unwrap() = 0xff;
    assert!(matches!(
        decode::<IncomingRequest, _>(invalid.as_slice(), &policy()),
        Err(TransferError::Malformed(_))
    ));

    #[derive(Serialize)]
    enum Tagged {
        Future(ID),
    }
    let tagged = encode(&Tagged::Future(ID::from_bytes("tagged")));
    assert!(matches!(
        decode::<IncomingRequest, _>(tagged.as_slice(), &policy()),
        Ok(IncomingRequest::Unsupported)
    ));
}

#[test]
fn inconsistent_policies_are_rejected() {
    for change in [
        (|policy: &mut TransferPolicy| policy.max_message_bytes = 0) as fn(&mut TransferPolicy),
        |policy| policy.max_message_bytes = policy.max_reserved_bytes + 1,
        |policy| policy.max_inbound_messages = 0,
        |policy| policy.max_depth = 0,
        |policy| policy.service_queue_slots = 0,
        |policy| policy.progress_window = Duration::ZERO,
        |policy| policy.overall_timeout = Some(Duration::ZERO),
    ] {
        let mut policy = policy();
        change(&mut policy);
        assert!(TransferBudget::new(policy).is_err());
    }
}

#[tokio::test]
async fn large_messages_stream_and_messages_stay_delimited_on_one_socket() {
    let (mut near, mut far) = duplex(4 * 1024);
    let sender = TransferBudget::default();
    let receiver = TransferBudget::default();
    let large = entry_with(3 * MIB);

    let sending = {
        let sender = sender.clone();
        let large = large.clone();
        tokio::spawn(async move {
            send(&mut near, SyncRequest::SendEntries(vec![large]), &sender)
                .await
                .unwrap();
            send(&mut near, SyncResponse::Count(1), &sender)
                .await
                .unwrap();
            send(&mut near, SyncResponse::Count(2), &sender)
                .await
                .unwrap();
            near
        })
    };
    let request: SyncRequest = receive(&mut far, &receiver).await.unwrap();
    assert!(matches!(request, SyncRequest::SendEntries(entries) if entries == vec![large]));
    // Each read stopped at its message: the following ones are still intact.
    assert!(matches!(
        receive::<SyncResponse>(&mut far, &receiver).await.unwrap(),
        SyncResponse::Count(1)
    ));
    assert!(matches!(
        receive::<SyncResponse>(&mut far, &receiver).await.unwrap(),
        SyncResponse::Count(2)
    ));
    sending.await.unwrap();
    wait_until_idle(&sender).await;
    wait_until_idle(&receiver).await;
}

#[tokio::test]
async fn clean_eof_and_truncation_are_different() {
    let budget = TransferBudget::default();
    let options = || ReadOptions {
        allow_clean_eof: true,
        ..Default::default()
    };
    let mut empty: &[u8] = &[];
    assert!(
        read_message::<_, SyncResponse>(&mut empty, &budget, options())
            .await
            .unwrap()
            .is_none()
    );
    let mut partial: &[u8] = &[0, 0];
    assert!(matches!(
        read_message::<_, SyncResponse>(&mut partial, &budget, options()).await,
        Err(TransferError::Truncated)
    ));

    // Truncated in a streamed body: the worker exits and admission returns.
    let mut header = (10 * MIB as u64).to_be_bytes().to_vec();
    header.extend_from_slice(&byte_string_header(10 * MIB - 5));
    header.extend_from_slice(&vec![0; 2 * INLINE_MESSAGE_BYTES]);
    let mut reader = header.as_slice();
    assert!(matches!(
        read_message::<_, ByteBuf>(&mut reader, &budget, options()).await,
        Err(TransferError::Truncated)
    ));
    wait_until_idle(&budget).await;

    let mut zero: &[u8] = &[0; 8];
    assert!(matches!(
        read_message::<_, SyncResponse>(&mut zero, &budget, options()).await,
        Err(TransferError::Malformed(_))
    ));
}

#[tokio::test]
async fn stream_ends_and_declared_lengths_are_checked() {
    let budget = TransferBudget::default();
    let encoded = encode(&SyncResponse::Count(1));
    let mut message = (encoded.len() as u64).to_be_bytes().to_vec();
    message.extend_from_slice(&encoded);

    let mut extra = message.clone();
    extra.push(0);
    let options = ReadOptions {
        require_end: true,
        ..Default::default()
    };
    assert!(matches!(
        read_message::<_, SyncResponse>(&mut extra.as_slice(), &budget, options).await,
        Err(TransferError::Malformed(_))
    ));

    let options = ReadOptions {
        declared_stream_bytes: Some(message.len() as u64 + 1),
        ..Default::default()
    };
    assert!(matches!(
        read_message::<_, SyncResponse>(&mut message.as_slice(), &budget, options).await,
        Err(TransferError::Malformed(_))
    ));
}

#[tokio::test]
async fn admission_refuses_overload_without_touching_the_stream() {
    let budget = budget_with(|policy| {
        policy.max_message_bytes = 2 * MIB as u64;
        policy.max_reserved_bytes = 3 * MIB as u64;
        policy.max_outbound_messages = 2;
        policy.max_inbound_messages = 1;
    });

    // Sized, not sent: the limit applies with nothing written.
    let too_large = prepare_message(vec![entry_with(3 * MIB)], &budget).await;
    assert_eq!(refused(too_large), ResourceLimit::MessageBytes);
    wait_until_idle(&budget).await;

    // A declared size is refused before anything is allocated for it.
    let mut declared: &[u8] = &(3 * MIB as u64).to_be_bytes();
    assert_eq!(
        refused(
            read_message::<_, SyncResponse>(&mut declared, &budget, ReadOptions::default()).await
        ),
        ResourceLimit::MessageBytes
    );

    let first = prepare_message(vec![entry_with(MIB + MIB / 2)], &budget)
        .await
        .unwrap();
    let second = prepare_message(vec![entry_with(MIB + MIB / 2)], &budget).await;
    assert_eq!(refused(second), ResourceLimit::ReservedBytes);
    let third = prepare_message(vec![entry_with(MIB)], &budget)
        .await
        .unwrap();
    let fourth = prepare_message(vec![entry_with(MIB)], &budget).await;
    assert_eq!(refused(fourth), ResourceLimit::OutboundMessages);

    // Inline messages need no slot, so small traffic is unaffected by overload.
    prepare_message(SyncResponse::Ack, &budget).await.unwrap();

    drop((first, third));
    wait_until_idle(&budget).await;
    assert!(
        prepare_message(vec![entry_with(MIB)], &budget)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn reservations_cover_a_message_until_its_owner_drops_it() {
    let budget = budget_with(|policy| policy.max_inbound_messages = 1);
    let (mut near, mut far) = duplex(4 * 1024);
    tokio::spawn(async move {
        let budget = TransferBudget::default();
        send(&mut near, vec![entry_with(MIB)], &budget)
            .await
            .unwrap();
        send(&mut near, vec![entry_with(MIB)], &budget)
            .await
            .unwrap();
        near
    });
    let first = read_message::<_, Vec<Entry>>(&mut far, &budget, ReadOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(budget.usage().inbound_messages, 1);
    assert!(budget.usage().reserved_bytes > MIB as u64);

    let second = read_message::<_, Vec<Entry>>(&mut far, &budget, ReadOptions::default()).await;
    assert_eq!(refused(second), ResourceLimit::InboundMessages);

    drop(first);
    wait_until_idle(&budget).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocked_peer_blocks_only_its_own_message() {
    let budget = TransferBudget::default();
    let (mut near, far) = duplex(1024);

    // The peer never reads, so the encoder must be parked on its bounded queue.
    let writer = tokio::spawn({
        let budget = budget.clone();
        async move { send(&mut near, vec![entry_with(4 * MIB)], &budget).await }
    });

    // This single-threaded runtime keeps running other work meanwhile.
    let ticks = AtomicUsize::new(0);
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(1)).await;
        ticks.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(ticks.load(Ordering::Relaxed), 20);
    assert_eq!(budget.usage().outbound_messages, 1);

    // Cancelling the transfer stops the worker, and only then frees admission.
    writer.abort();
    let _ = writer.await;
    wait_until_idle(&budget).await;
    drop(far);
}

#[tokio::test]
async fn abandoned_output_stops_its_worker_and_frees_admission() {
    let budget = TransferBudget::default();
    let (mut near, far) = duplex(1024);
    let monitor = ProgressMonitor::new(budget.policy());
    let message = prepare_message(vec![entry_with(4 * MIB)], &budget)
        .await
        .unwrap();
    assert_eq!(budget.usage().outbound_messages, 1);

    let sending = message.write_to(&mut near, &monitor);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), sending)
            .await
            .is_err(),
        "the peer is not reading, so this cannot finish"
    );
    wait_until_idle(&budget).await;
    drop(far);
}

#[tokio::test]
async fn abandoned_input_stops_its_worker_and_frees_admission() {
    let budget = TransferBudget::default();
    let (mut near, mut far) = duplex(4 * CHUNK_BYTES);
    let declared = 20 * MIB as u64;
    near.write_all(&declared.to_be_bytes()).await.unwrap();
    near.write_all(&byte_string_header(declared as usize - 5))
        .await
        .unwrap();
    near.write_all(&vec![0; CHUNK_BYTES]).await.unwrap();

    let reading = tokio::spawn({
        let budget = budget.clone();
        async move { read_message::<_, ByteBuf>(&mut far, &budget, ReadOptions::default()).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.usage().inbound_messages == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(budget.usage().reserved_bytes, declared);

    reading.abort();
    let _ = reading.await;
    wait_until_idle(&budget).await;
}

#[tokio::test]
async fn concurrent_transfers_stay_separate() {
    let budget = TransferBudget::default();
    let mut transfers = Vec::new();
    for index in 0..4usize {
        let budget = budget.clone();
        transfers.push(tokio::spawn(async move {
            let (mut near, mut far) = duplex(16 * 1024);
            let entry = entry_with(MIB + index);
            let sent = entry.clone();
            let writer = {
                let budget = TransferBudget::default();
                tokio::spawn(async move { send(&mut near, vec![sent], &budget).await })
            };
            let received: Vec<Entry> = receive(&mut far, &budget).await.unwrap();
            writer.await.unwrap().unwrap();
            assert_eq!(received, vec![entry]);
        }));
    }
    for transfer in transfers {
        transfer.await.unwrap();
    }
    wait_until_idle(&budget).await;
}

#[tokio::test]
async fn inline_encoding_is_bounded() {
    assert!(encode_inline(&SyncResponse::Ack).is_some());
    assert!(encode_inline(&vec![entry_with(INLINE_MESSAGE_BYTES)]).is_none());
}

fn enveloped<T: Serialize>(value: &T) -> Vec<u8> {
    let body = encode(value);
    let mut message = (body.len() as u64).to_be_bytes().to_vec();
    message.extend_from_slice(&body);
    message
}

/// Feed `message` to a peer at a fixed virtual rate.
///
/// A live codec worker stops the paused clock advancing by itself, so each step
/// moves it explicitly. A write that is not consumed holds the clock back too,
/// which keeps the measured rate independent of how fast this machine runs.
async fn deliver(near: &mut DuplexStream, message: &[u8], step: usize, pause: Duration) {
    for chunk in message.chunks(step) {
        tokio::time::advance(pause).await;
        if near.write_all(chunk).await.is_err() {
            return;
        }
    }
}

// Virtual time: a transfer of minutes completes instantly and deterministically.
#[tokio::test(start_paused = true)]
async fn healthy_download_runs_past_the_old_total_deadline() {
    let budget = TransferBudget::default();
    let (mut near, mut far) = duplex(8 * 1024);
    let message = enveloped(&vec![entry_with(5 * MIB)]);
    let started = tokio::time::Instant::now();
    // 8 KiB per 100 ms: 80 KiB/s, above the 64 KiB/s floor, for over a minute.
    let feed = deliver(&mut near, &message, 8 * 1024, Duration::from_millis(100));
    let read = read_message::<_, Vec<Entry>>(&mut far, &budget, ReadOptions::default());
    let ((), received) = tokio::join!(feed, read);
    assert!(started.elapsed() > Duration::from_secs(30));
    assert_eq!(received.unwrap().unwrap().value.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn slow_trickle_fails_a_real_download() {
    let budget = TransferBudget::default();
    let (mut near, mut far) = duplex(1024);
    let message = enveloped(&vec![entry_with(MIB)]);
    let started = tokio::time::Instant::now();
    let read = read_message::<_, Vec<Entry>>(&mut far, &budget, ReadOptions::default());
    let failed = tokio::select! {
        failed = read => failed,
        () = deliver(&mut near, &message, 64, Duration::from_secs(1)) => panic!("not refused"),
    };
    assert!(
        matches!(failed, Err(TransferError::InsufficientProgress { .. })),
        "{:?}",
        failed.err()
    );
    // Never silent for long, so this is the rate check and not the stall one.
    assert!(started.elapsed() < Duration::from_secs(30));
    wait_until_idle(&budget).await;
}

#[tokio::test(start_paused = true)]
async fn stalled_download_fails_and_frees_admission() {
    let budget = TransferBudget::default();
    let (mut near, mut far) = duplex(1024);
    let message = enveloped(&vec![entry_with(2 * MIB)]);
    let started = tokio::time::Instant::now();
    let read = read_message::<_, Vec<Entry>>(&mut far, &budget, ReadOptions::default());
    let feed_then_fall_silent = async {
        // Enough to satisfy the first window, all before the clock moves.
        near.write_all(&message[..MIB]).await.unwrap();
        loop {
            tokio::time::advance(Duration::from_secs(1)).await;
        }
    };
    let failed = tokio::select! {
        failed = read => failed,
        () = feed_then_fall_silent => unreachable!(),
    };
    assert!(
        matches!(failed, Err(TransferError::Stalled { .. })),
        "{:?}",
        failed.err()
    );
    assert!(started.elapsed() >= Duration::from_secs(30));
    wait_until_idle(&budget).await;
    drop(near);
}

#[tokio::test(start_paused = true)]
async fn upload_to_a_peer_that_stops_reading_fails() {
    let budget = TransferBudget::default();
    let (mut near, far) = duplex(64 * 1024);
    let monitor = ProgressMonitor::new(budget.policy());
    let message = prepare_message(vec![entry_with(MIB)], &budget)
        .await
        .unwrap();
    let time_passes = async {
        loop {
            tokio::time::advance(Duration::from_secs(1)).await;
        }
    };
    let failed = tokio::select! {
        failed = message.write_to(&mut near, &monitor) => failed,
        () = time_passes => unreachable!(),
    };
    assert!(
        matches!(
            failed,
            Err(TransferError::Stalled { .. } | TransferError::InsufficientProgress { .. })
        ),
        "{:?}",
        failed.err()
    );
    wait_until_idle(&budget).await;
    drop(far);
}

/// Time spent decoding after the last byte is not the peer falling behind.
#[tokio::test]
async fn a_finished_body_is_not_timed_while_it_decodes() {
    struct SlowToBuild;
    impl<'de> Deserialize<'de> for SlowToBuild {
        fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
            ByteBuf::deserialize(decoder)?;
            std::thread::sleep(Duration::from_millis(600));
            Ok(Self)
        }
    }

    let budget = budget_with(|policy| policy.stall_timeout = Duration::from_millis(300));
    let (mut near, mut far) = duplex(MIB);
    let message = enveloped(&ByteBuf::from(vec![0u8; 2 * INLINE_MESSAGE_BYTES]));
    near.write_all(&message).await.unwrap();

    read_message::<_, SlowToBuild>(&mut far, &budget, ReadOptions::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn sizing_a_message_has_its_own_deadline() {
    struct Slow;
    impl Serialize for Slow {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            // Past the inline size, and slow to serialize.
            std::thread::sleep(Duration::from_millis(100));
            serializer.serialize_bytes(&vec![0u8; 2 * INLINE_MESSAGE_BYTES])
        }
    }
    let budget = budget_with(|policy| policy.preparation_timeout = Duration::from_millis(1));
    let failed = prepare_message(Slow, &budget).await;
    assert!(
        matches!(failed, Err(TransferError::PreparationTimeout { .. })),
        "{:?}",
        failed.err().map(|e| e.to_string())
    );
    wait_until_idle(&budget).await;
}
