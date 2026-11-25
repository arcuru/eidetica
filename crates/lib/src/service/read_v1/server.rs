use std::{io, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, StreamExt};
use prost::bytes::Bytes;
use tokio::{
    sync::{Semaphore, watch},
    time::timeout,
};
use tonic::{Request, Response, Status, Streaming, transport::Server};

use crate::{
    Instance,
    auth::generate_challenge,
    service::server::BoundSocket,
    state::{
        ReadScope, StateAdmission, StateError, StateOperation, StateReader,
        work::{ReadWork, Walk},
    },
};

use super::{ReadError, car, wire::*};

const PROOF_TIMEOUT: Duration = Duration::from_secs(3);
const WORK_TIMEOUT: Duration = Duration::from_secs(30);
const STATE_ENTRIES: usize = 2048;
const ACTIVE_READS: usize = 4;

/// Owner of a distinct, read-only Unix socket. Uses the same path, locking and
/// permission rules as the existing local service; does not open another backend.
pub struct ReadServer {
    socket: BoundSocket,
    service: ReadService,
}

impl ReadServer {
    pub async fn bind(instance: Instance, path: impl Into<PathBuf>) -> crate::Result<Self> {
        instance.require_local_engine()?;
        Ok(Self {
            socket: BoundSocket::bind(path.into()).await?,
            service: ReadService {
                instance,
                producer: *uuid::Uuid::new_v4().as_bytes(),
                reads: Arc::new(Semaphore::new(ACTIVE_READS)),
                admission: None,
            },
        })
    }

    /// Supply the existing engine's independent computation decision. It can be
    /// called before backend work and again at each delivery checkpoint.
    pub fn with_state_admission(mut self, admission: Arc<StateAdmission>) -> Self {
        self.service.admission = Some(admission);
        self
    }

    /// Serve until the shutdown watch changes or is dropped.
    pub async fn run(
        self,
        mut shutdown: watch::Receiver<()>,
    ) -> Result<(), tonic::transport::Error> {
        let incoming = async_stream::stream! {
            loop {
                yield self.socket.listener.accept().await.map(|(socket, _)| socket);
            }
        };
        Server::builder()
            .max_concurrent_streams(16)
            .max_connection_age(Duration::from_secs(300))
            .max_connection_age_grace(PROOF_TIMEOUT)
            .add_service(
                reads_server::ReadsServer::new(self.service)
                    .max_decoding_message_size(MESSAGE_BYTES)
                    .max_encoding_message_size(MESSAGE_BYTES),
            )
            .serve_with_incoming_shutdown(incoming, async move {
                let _ = shutdown.changed().await;
            })
            .await
    }
}

type ReadFrames = Pin<Box<dyn Stream<Item = Result<ReadFrame, Status>> + Send>>;

struct ReadService {
    instance: Instance,
    producer: [u8; 16],
    reads: Arc<Semaphore>,
    admission: Option<Arc<StateAdmission>>,
}

#[tonic::async_trait]
impl reads_server::Reads for ReadService {
    type ReadStream = ReadFrames;

    async fn get_capabilities(
        &self,
        _: Request<CapabilityRequest>,
    ) -> Result<Response<Capabilities>, Status> {
        Ok(Response::new(Capabilities {
            revision: REVISION,
            producer: self.producer.to_vec(),
            chunk_bytes: CHUNK_BYTES as u32,
            object_bytes: OBJECT_BYTES as u32,
            history_entries: HISTORY_ENTRIES as u32,
            history_bytes: HISTORY_BYTES as u64,
        }))
    }

    async fn read(
        &self,
        request: Request<Streaming<ReadInput>>,
    ) -> Result<Response<ReadFrames>, Status> {
        // Acquire before waiting on an untrusted request stream. Capability
        // reads bypass this pool and so can't queue behind bulk producers.
        let permit = self
            .reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("active read limit"))?;
        let mut input = request.into_inner();
        let instance = self.instance.clone();
        let admission = self.admission.clone();
        let producer = self.producer;
        let response = async_stream::try_stream! {
            // This is the response body future, not a detached producer. A
            // reset/drop releases the permit and abandons the backend work.
            let _permit = permit;
            let query = match next_input(&mut input).await? {
                Some(read_input::Frame::Query(q)) => q,
                _ => Err(Status::invalid_argument("expected read query"))?,
            };
            let plan = match Plan::parse(query) {
                Ok(plan) => plan,
                Err(error) => { yield failure(error)?; return; }
            };
            let challenge = Challenge { producer: producer.to_vec(), nonce: generate_challenge() };
            yield frame(read_frame::Frame::Challenge(challenge.clone()));
            let signature = match next_input(&mut input).await? {
                Some(read_input::Frame::Signature(signature)) => signature,
                _ => Err(Status::invalid_argument("expected proof"))?,
            };
            if plan.key.verify(&plan.transcript(&challenge), &signature).is_err() {
                yield failure(ReadError::Denied)?;
                return;
            }
            if next_input(&mut input).await?.is_some() {
                Err(Status::invalid_argument("request must end after proof"))?;
            }
            drop(input);
            let mut body = std::pin::pin!(produce(instance, admission, producer, plan));
            while let Some(item) = body.next().await {
                match item {
                    Ok(frame) => yield frame,
                    Err(error) => { yield failure(error)?; break; }
                }
            }
        };
        Ok(Response::new(Box::pin(response)))
    }
}

async fn next_input(input: &mut Streaming<ReadInput>) -> Result<Option<read_input::Frame>, Status> {
    timeout(PROOF_TIMEOUT, input.message())
        .await
        .map_err(|_| Status::deadline_exceeded("read proof timed out"))??
        .map(|m| {
            m.frame
                .ok_or_else(|| Status::invalid_argument("unknown input frame"))
        })
        .transpose()
}

fn frame(frame: read_frame::Frame) -> ReadFrame {
    ReadFrame { frame: Some(frame) }
}

fn data(bytes: &[u8]) -> ReadFrame {
    // Do not let a tiny in-flight chunk retain the entire large object.
    frame(read_frame::Frame::Data(Bytes::copy_from_slice(bytes)))
}

fn terminal(outcome: Outcome, context: Option<Context>) -> ReadFrame {
    frame(read_frame::Frame::End(End {
        outcome: outcome.into(),
        context,
    }))
}

fn failure(error: ReadError) -> Result<ReadFrame, Status> {
    let outcome = match error {
        ReadError::Refused => Outcome::Refused,
        ReadError::Denied => Outcome::Denied,
        ReadError::InvalidBoundary => Outcome::InvalidBoundary,
        ReadError::UnavailableHistory => Outcome::UnavailableHistory,
        ReadError::Unsupported => Outcome::Unsupported,
        ReadError::Limit => Outcome::Limit,
        ReadError::InvalidData(_) => return Err(Status::invalid_argument("invalid read data")),
        _ => return Err(Status::internal("read did not complete")),
    };
    Ok(terminal(outcome, None))
}

fn engine_error(error: crate::Error) -> ReadError {
    match error {
        crate::Error::State(StateError::Refused) => ReadError::Refused,
        crate::Error::State(StateError::Denied) => ReadError::Denied,
        crate::Error::State(StateError::InvalidBoundary) => ReadError::InvalidBoundary,
        crate::Error::State(StateError::UnavailableHistory) => ReadError::UnavailableHistory,
        crate::Error::State(StateError::UnsupportedProjection) => ReadError::Unsupported,
        crate::Error::State(StateError::Limit) => ReadError::Limit,
        err => {
            tracing::debug!(error = %err.module(), "v1 read engine failed");
            ReadError::Incomplete
        }
    }
}

fn produce(
    instance: Instance,
    admission: Option<Arc<StateAdmission>>,
    producer: [u8; 16],
    plan: Plan,
) -> impl Stream<Item = Result<ReadFrame, ReadError>> {
    async_stream::try_stream! {
        // Native credential records do not gain a second release path via
        // whole-Entry history or a targeted projection.
        if &plan.database == instance.users_db_id() {
            Err(ReadError::Denied)?;
        }
        let history = matches!(plan.operation, Operation::History { .. });
        let (entries, bytes) = if history {
            (HISTORY_ENTRIES, HISTORY_BYTES)
        } else {
            (STATE_ENTRIES, OBJECT_BYTES)
        };
        let work = ReadWork::new(entries, bytes, OBJECT_BYTES, WORK_TIMEOUT);
        let reader = StateReader::from_proven_signer(&instance, plan.key.clone(), &plan.identity)
            .with_work(&work).with_admission(admission.as_deref());
        let context = match &plan.operation {
            Operation::Capture => {
                let cut = reader.capture(&plan.database, ReadScope::Verified).await.map_err(engine_error)?;
                let context = plan.wire_context(producer, cut.snapshot());
                yield frame(read_frame::Frame::Start(context.clone()));
                context
            }
            Operation::State { at, store } => {
                let projection = reader.read_doc(&plan.context(at), store).await.map_err(engine_error)?;
                let body = encode_doc(&projection.value)?;
                drop(projection);
                let context = plan.wire_context(producer, at);
                yield frame(read_frame::Frame::Start(context.clone()));
                // The policy is independent of authorization. It may stop an
                // in-flight computation result, but never deliver a foreign one.
                for chunk in body.chunks(CHUNK_BYTES) {
                    reader.admit(&plan.database, StateOperation::ReadDoc { store }).map_err(engine_error)?;
                    work.checkpoint().await.map_err(engine_error)?;
                    yield data(chunk);
                }
                reader.admit(&plan.database, StateOperation::ReadDoc { store }).map_err(engine_error)?;
                context
            }
            Operation::History { at } => {
                reader.authorize_history(&plan.context(at)).await.map_err(engine_error)?;
                let backend = instance.require_local_engine().map_err(engine_error)?;
                let context = plan.wire_context(producer, at);
                yield frame(read_frame::Frame::Start(context.clone()));
                let mut prefix = unsigned_varint::encode::usize_buffer();
                let header = car::header(at)?;
                yield data(unsigned_varint::encode::usize(header.len(), &mut prefix));
                yield data(&header);
                let mut walk = Walk::new(at);
                let mut remaining = HISTORY_BYTES;
                while let Some(entry) = walk.next(&work, &*backend, &plan.database).await.map_err(engine_error)? {
                    let (cid, bytes) = car::entry_body(&entry)?;
                    drop(entry);
                    let length = cid.len() + bytes.len();
                    remaining = remaining.checked_sub(length).ok_or(ReadError::Limit)?;
                    yield data(unsigned_varint::encode::usize(length, &mut prefix));
                    yield data(&cid);
                    for chunk in bytes.chunks(CHUNK_BYTES) {
                        work.checkpoint().await.map_err(engine_error)?;
                        yield data(chunk);
                    }
                }
                context
            }
        };
        work.checkpoint().await.map_err(engine_error)?;
        yield terminal(Outcome::Complete, Some(context));
    }
}

fn encode_doc(doc: &crate::crdt::Doc) -> Result<Vec<u8>, ReadError> {
    // Limit output *while serializing*, independently of the fold's budget.
    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        overflow: bool,
    }
    impl io::Write for Output {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.bytes.len() + buf.len() > OBJECT_BYTES {
                self.overflow = true;
                return Err(io::Error::other("projection too large"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Output::default();
    serde_json::to_writer(&mut writer, doc).map_err(|_| {
        if writer.overflow {
            ReadError::Limit
        } else {
            ReadError::Incomplete
        }
    })?;
    Ok(writer.bytes)
}
