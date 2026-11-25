use std::{pin::Pin, time::Duration};

use futures_util::Stream;
use prost::bytes::Bytes;
use tokio::{net::UnixListener, sync::oneshot, time::timeout};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

use crate::{
    DatabaseKey, ID, Snapshot,
    auth::{PrivateKey, PublicKey, verify_entry_signature},
};

use super::{ReadClient, ReadError, wire::*};

const DB: &str = "bafyr4iac6a3ogzhcoaxeyretr36jxbzuulsmniwbgqh34livdmfphskawq";
const TIP: &str = "bafyr4ibjn2smxkt2elzmt77eyz7ghe2xularxakepduwve3rml5kgjc4nu";
const PUBLIC: &str = "ed25519:+AzM3OSuHAeuIIoq35mjEK5CB+Awb6AjYRCwaCe7uNA=";
const PRODUCER: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const PROOF: &str = concat!(
    "53323e0f09428c84f06767d1b825837f8a1928a85a834c80aefccac4ccf79bec0f",
    "d475deb5867ddf5722431bb780b84edb26dea9224ef9b2fa28a39194193006",
);

fn challenge() -> Challenge {
    Challenge {
        producer: PRODUCER.to_vec(),
        nonce: (16..48).collect(),
    }
}

fn state_query() -> Query {
    Query {
        revision: REVISION,
        database: DB.to_string(),
        public_key: PUBLIC.to_string(),
        identity: Some(SignerIdentity {
            hint: Some(signer_identity::Hint::Key(PUBLIC.to_string())),
            delegation: Vec::new(),
        }),
        require: Vec::new(),
        operation: Some(query::Operation::State(State {
            at: Some(tip_wire()),
            store: "data".into(),
            projection: "eidetica/doc:v0".into(),
        })),
    }
}

fn tip_wire() -> super::wire::Snapshot {
    super::wire::Snapshot {
        tips: vec![TIP.to_string()],
    }
}

#[test]
fn test_v1_proof_matches_independent_vector_and_binds_context() {
    let q = state_query();
    let key = PublicKey::from_prefixed_string(PUBLIC).unwrap();
    let proof = hex::decode(PROOF).unwrap();
    let verify = |query, challenge: &Challenge| {
        key.verify(&Plan::parse(query).unwrap().transcript(challenge), &proof)
    };
    verify(q.clone(), &challenge()).unwrap();
    let mut changed = challenge();
    changed.nonce[0] ^= 1;
    assert!(verify(q.clone(), &changed).is_err());
    changed = challenge();
    changed.producer[0] ^= 1;
    assert!(verify(q.clone(), &changed).is_err());
    let mut history = q.clone();
    history.operation = Some(query::Operation::History(History {
        at: Some(tip_wire()),
    }));
    assert!(verify(history, &challenge()).is_err());
    let mut other = q.clone();
    other.database = TIP.to_string();
    assert!(verify(other, &challenge()).is_err());
    let mut other = q.clone();
    let Some(query::Operation::State(state)) = other.operation.as_mut() else {
        unreachable!()
    };
    state.store.push_str("-other");
    assert!(verify(other, &challenge()).is_err());
    let mut same = q.clone();
    let Some(query::Operation::State(state)) = same.operation.as_mut() else {
        unreachable!()
    };
    state.at.as_mut().unwrap().tips.push(TIP.to_string());
    verify(same, &challenge()).unwrap();
    let mut unsupported = q;
    unsupported.require = vec!["unimplemented".into()];
    assert!(matches!(
        Plan::parse(unsupported),
        Err(ReadError::Unsupported)
    ));
}

#[derive(Clone, Copy)]
enum Script {
    MissingTerminal,
    LostTrailers,
    IntactCar,
    OmittedRoot,
    OversizedSection,
}

#[tonic::async_trait]
impl reads_server::Reads for Script {
    type ReadStream = Pin<Box<dyn Stream<Item = Result<ReadFrame, Status>> + Send>>;

    async fn get_capabilities(
        &self,
        _: Request<CapabilityRequest>,
    ) -> Result<Response<Capabilities>, Status> {
        Ok(Response::new(Capabilities {
            revision: REVISION,
            producer: PRODUCER.to_vec(),
            chunk_bytes: CHUNK_BYTES as u32,
            object_bytes: OBJECT_BYTES as u32,
            history_entries: HISTORY_ENTRIES as u32,
            history_bytes: HISTORY_BYTES as u64,
        }))
    }

    async fn read(
        &self,
        request: Request<Streaming<ReadInput>>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        use read_frame::Frame;
        let script = *self;
        let mut input = request.into_inner();
        Ok(Response::new(Box::pin(async_stream::try_stream! {
            let Some(read_input::Frame::Query(query)) = input.message().await?.and_then(|m| m.frame) else {
                Err(Status::invalid_argument("expected query"))?; return;
            };
            let plan = Plan::parse(query).unwrap();
            let frame = |frame| ReadFrame { frame: Some(frame) };
            let challenge = challenge();
            yield frame(Frame::Challenge(challenge.clone()));
            let Some(read_input::Frame::Signature(proof)) = input.message().await?.and_then(|m| m.frame) else {
                Err(Status::invalid_argument("expected proof"))?; return;
            };
            plan.key.verify(&plan.transcript(&challenge), &proof).unwrap();
            let final_input = input.message().await?;
            assert!(final_input.is_none());
            let context = plan.wire_context(PRODUCER, &Snapshot::EMPTY);
            yield frame(Frame::Start(context.clone()));
            match script {
                Script::MissingTerminal | Script::LostTrailers => {
                    yield frame(Frame::Data(Bytes::from_static(br#"{"children":{}}"#)));
                    if matches!(script, Script::MissingTerminal) { return; }
                }
                Script::IntactCar | Script::OmittedRoot | Script::OversizedSection => {
                    let mut car = hex::decode(super::car_fixture::CAR).unwrap();
                    let (header, rem) = unsigned_varint::decode::usize(&car).unwrap();
                    let end_header = car.len() - rem.len() + header;
                    if matches!(script, Script::OmittedRoot) {
                        let (first, rem) = unsigned_varint::decode::usize(&car[end_header..]).unwrap();
                        car.truncate(car.len() - rem.len() + first);
                    } else if matches!(script, Script::OversizedSection) {
                        car.truncate(end_header);
                        let mut prefix = unsigned_varint::encode::usize_buffer();
                        car.extend_from_slice(unsigned_varint::encode::usize(OBJECT_BYTES * 2, &mut prefix));
                    }
                    // Force section prefixes, binary CIDs and Entry CBOR to cross
                    // Data record boundaries independent of the actual sizes.
                    for byte in car {
                        yield frame(Frame::Data(Bytes::copy_from_slice(&[byte])));
                    }
                }
            }
            yield frame(Frame::End(End { outcome: Outcome::Complete.into(), context: Some(context) }));
            if matches!(script, Script::LostTrailers) {
                tokio::task::yield_now().await;
                Err(Status::unavailable("test transport loss after terminal"))?;
            }
        })))
    }
}

async fn exercise(script: Script, check: impl AsyncFnOnce(ReadClient)) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("read.sock");
    let incoming = UnixListenerStream::new(UnixListener::bind(&socket).unwrap());
    let (signal, shutdown) = oneshot::channel::<()>();
    let task = tokio::spawn(
        Server::builder()
            .add_service(reads_server::ReadsServer::new(script))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown.await;
            }),
    );
    let client = ReadClient::connect(socket).await.unwrap();
    check(client).await;
    drop(signal);
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn fixture_context() -> (DatabaseKey, ID, Snapshot) {
    let signer = DatabaseKey::new(PrivateKey::from_bytes("ed25519", &[0x53; 32]).unwrap());
    (
        signer,
        ID::parse(DB).unwrap(),
        Snapshot::from([ID::parse(TIP).unwrap()]),
    )
}

#[tokio::test]
async fn test_v1_sdk_requires_terminal_and_clean_grpc_status() {
    for script in [Script::MissingTerminal, Script::LostTrailers] {
        exercise(script, async |client| {
            let (key, db, at) = fixture_context();
            let mut stream = client.read_state(&key, &db, &at, "data").await.unwrap();
            assert!(stream.next_chunk().await.unwrap().is_some());
            assert!(matches!(
                stream.next_chunk().await,
                Err(ReadError::Incomplete)
            ));
            assert!(matches!(stream.finish(), Err(ReadError::Incomplete)));
            assert!(matches!(
                client
                    .read_state(&key, &db, &at, "data")
                    .await
                    .unwrap()
                    .collect_complete()
                    .await,
                Err(ReadError::Incomplete)
            ));
        })
        .await;
    }
}

#[tokio::test]
async fn test_v1_sdk_reads_signed_car_fixture_and_requires_coverage() {
    exercise(Script::IntactCar, async |client| {
        let (key, db, at) = fixture_context();
        let result = client
            .history(&key, &db, &at)
            .await
            .unwrap()
            .collect_complete()
            .await
            .unwrap();
        assert_eq!(result.value.len(), 2);
        assert_eq!(result.context.at.snapshot(), &at);
        for entry in result.value {
            verify_entry_signature(&entry, &key.public_key()).unwrap();
        }
    })
    .await;
    exercise(Script::OmittedRoot, async |client| {
        let (key, db, at) = fixture_context();
        let mut history = client.history(&key, &db, &at).await.unwrap();
        assert_eq!(
            history.next_entry().await.unwrap().unwrap().id_ref(),
            &at.tips()[0]
        );
        assert!(matches!(
            history.next_entry().await,
            Err(ReadError::Incomplete)
        ));
        assert!(matches!(history.finish(), Err(ReadError::Incomplete)));
    })
    .await;
    exercise(Script::OversizedSection, async |client| {
        let (key, db, at) = fixture_context();
        let mut history = client.history(&key, &db, &at).await.unwrap();
        assert!(matches!(history.next_entry().await, Err(ReadError::Limit)));
    })
    .await;
}

#[tokio::test]
async fn test_v1_server_proof_is_rpc_local() {
    use crate::{Database, Instance, NewAccount, backend::database::InMemory, crdt::Doc};
    use tokio::sync::{mpsc, watch};
    use tokio_stream::wrappers::ReceiverStream;
    use tonic::transport::Endpoint;

    let (instance, _) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::passwordless("v1-admin"),
    )
    .await
    .unwrap();
    let (signer, _, _) = fixture_context();
    let db = Database::create(&instance, signer.clone(), Doc::new())
        .await
        .unwrap();
    let mut query = state_query();
    query.database = db.root_id().to_string();
    query.operation = Some(query::Operation::Capture(Capture {}));
    let plan = Plan::parse(query.clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read.sock");
    let server = super::ReadServer::bind(instance, &path).await.unwrap();
    let (stop, shutdown) = watch::channel(());
    let task = tokio::spawn(server.run(shutdown));
    let channel = Endpoint::from_shared(format!("unix:{}", path.display()))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = reads_client::ReadsClient::new(channel);
    let mut proof = None;
    for replay in [false, true] {
        let (send, recv) = mpsc::channel(1);
        send.try_send(ReadInput {
            frame: Some(read_input::Frame::Query(query.clone())),
        })
        .unwrap();
        let mut response = client
            .read(ReceiverStream::new(recv))
            .await
            .unwrap()
            .into_inner();
        let Some(read_frame::Frame::Challenge(challenge)) =
            response.message().await.unwrap().unwrap().frame
        else {
            panic!("expected proof challenge");
        };
        let signature =
            proof.get_or_insert_with(|| signer.signing_key().sign(&plan.transcript(&challenge)));
        send.send(ReadInput {
            frame: Some(read_input::Frame::Signature(signature.clone())),
        })
        .await
        .unwrap();
        drop(send);
        let reply = response.message().await.unwrap().unwrap().frame.unwrap();
        if replay {
            let read_frame::Frame::End(end) = reply else {
                panic!("previous RPC proof authorized another read")
            };
            assert_eq!(end.outcome, i32::from(Outcome::Denied));
        } else {
            assert!(matches!(reply, read_frame::Frame::Start(_)));
            let Some(read_frame::Frame::End(end)) =
                response.message().await.unwrap().unwrap().frame
            else {
                panic!("positive proof returned no terminal");
            };
            assert_eq!(end.outcome, i32::from(Outcome::Complete));
        }
        assert!(response.message().await.unwrap().is_none());
    }
    drop((client, stop));
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
