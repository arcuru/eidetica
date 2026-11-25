use std::{collections::HashSet, path::Path, time::Duration};

use prost::bytes::{Buf, Bytes};
use tokio::{
    sync::mpsc,
    time::{Instant, timeout_at},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Code, Status, Streaming,
    transport::{Channel, Endpoint},
};

use crate::{
    Entry, ID, Snapshot,
    crdt::{Codec, Doc},
    database::DatabaseKey,
    state::ComputedDoc,
};

use super::{
    Delivered, Projection, ReadError, Source, car,
    wire::{
        self, Capabilities, Challenge, Context, End, Outcome, Plan, ReadFrame, ReadInput,
        read_frame, read_input, reads_client::ReadsClient,
    },
};

const CALL_TIMEOUT: Duration = Duration::from_secs(35);

/// Client of a read-only v1 Unix endpoint. No login, credential bundle or
/// signer is stored in the connection. Each RPC proves its supplied key.
#[derive(Clone)]
pub struct ReadClient {
    rpc: ReadsClient<Channel>,
    producer: [u8; 16],
    limits: Limits,
}

#[derive(Clone, Copy)]
struct Limits {
    chunk: usize,
    object: usize,
    entries: usize,
    history: usize,
}

impl ReadClient {
    /// Connect and pin the capability reply to this serving incarnation. A
    /// reconnect to a restarted server requires a new `ReadClient`.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ReadError> {
        let path = path
            .as_ref()
            .to_str()
            .ok_or(ReadError::InvalidData("non-UTF-8 socket path"))?;
        let endpoint = Endpoint::from_shared(format!("unix:{path}"))?
            .connect_timeout(Duration::from_secs(3))
            .initial_stream_window_size(Some(2 * wire::CHUNK_BYTES as u32))
            .initial_connection_window_size(Some(8 * wire::CHUNK_BYTES as u32));
        let channel = endpoint.connect().await?;
        let mut rpc = ReadsClient::new(channel)
            .max_decoding_message_size(wire::MESSAGE_BYTES)
            .max_encoding_message_size(wire::MESSAGE_BYTES);
        let capabilities = timeout_at(
            Instant::now() + CALL_TIMEOUT,
            rpc.get_capabilities(wire::CapabilityRequest {}),
        )
        .await
        .map_err(|_| ReadError::Incomplete)?
        .map_err(status_error)?
        .into_inner();
        let Capabilities {
            revision,
            producer,
            chunk_bytes,
            object_bytes,
            history_entries,
            history_bytes,
        } = capabilities;
        if revision != wire::REVISION {
            return Err(ReadError::Unsupported);
        }
        let producer = array(producer)?;
        let limits = Limits {
            chunk: (chunk_bytes as usize).min(wire::CHUNK_BYTES),
            object: (object_bytes as usize).min(wire::OBJECT_BYTES),
            entries: (history_entries as usize).min(wire::HISTORY_ENTRIES),
            history: usize::try_from(history_bytes)
                .unwrap_or(usize::MAX)
                .min(wire::HISTORY_BYTES),
        };
        if [limits.chunk, limits.object, limits.entries, limits.history].contains(&0) {
            return Err(ReadError::InvalidData("invalid capability limits"));
        }
        Ok(Self {
            rpc,
            producer,
            limits,
        })
    }

    /// Acquire one Verified database-root cut. Current Read is required.
    pub async fn capture(&self, key: &DatabaseKey, database: &ID) -> Result<Source, ReadError> {
        let mut flow = self
            .open(
                key,
                database,
                wire::query::Operation::Capture(wire::Capture {}),
            )
            .await?;
        if flow.next().await?.is_some() {
            return Err(ReadError::InvalidData("capture has a body"));
        }
        flow.finish()?;
        flow.source()
    }

    /// Read a supported built-in Doc projection at an explicit Verified cut.
    /// Stream chunks are provisional; only `finish` after exhaustion succeeds.
    pub async fn read_state(
        &self,
        key: &DatabaseKey,
        database: &ID,
        at: &Snapshot,
        store: &str,
    ) -> Result<StateStream, ReadError> {
        let flow = self
            .open(
                key,
                database,
                wire::query::Operation::State(wire::State {
                    at: Some(wire::snapshot(at)),
                    store: store.to_owned(),
                    projection: ComputedDoc::PROJECTION.to_owned(),
                }),
            )
            .await?;
        let context = Projection {
            source: flow.source()?,
            store: flow.start.store.clone(),
            format: flow.start.projection.clone(),
        };
        Ok(StateStream {
            flow,
            context,
            remaining: self.limits.object,
            has_data: false,
        })
    }

    /// Stream canonical Entries of all branches at an explicit Verified cut.
    /// Current Read is required. Received Entries are candidates for local
    /// signature/authority validation, not independently Verified history.
    pub async fn history(
        &self,
        key: &DatabaseKey,
        database: &ID,
        at: &Snapshot,
    ) -> Result<HistoryStream, ReadError> {
        let flow = self
            .open(
                key,
                database,
                wire::query::Operation::History(wire::History {
                    at: Some(wire::snapshot(at)),
                }),
            )
            .await?;
        let context = flow.source()?;
        Ok(HistoryStream {
            flow,
            needed: context.at.snapshot().iter().cloned().collect(),
            context,
            seen: HashSet::new(),
            chunk: Bytes::new(),
            header_read: false,
            limits: self.limits,
            remaining: self.limits.history,
        })
    }

    async fn open(
        &self,
        key: &DatabaseKey,
        database: &ID,
        operation: wire::query::Operation,
    ) -> Result<Flow, ReadError> {
        let public = key.public_key();
        let query = wire::Query {
            revision: wire::REVISION,
            database: database.to_string(),
            public_key: public.to_prefixed_string(),
            identity: Some(wire::identity(key.identity(), &public)?),
            require: Vec::new(),
            operation: Some(operation),
        };
        let plan = Plan::parse(query.clone())?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        let (send, recv) = mpsc::channel(1);
        send.try_send(ReadInput {
            frame: Some(read_input::Frame::Query(query)),
        })
        .map_err(|_| ReadError::Incomplete)?;
        let mut rpc = self.rpc.clone();
        let mut request = tonic::Request::new(ReceiverStream::new(recv));
        request.set_timeout(CALL_TIMEOUT);
        let mut body = timeout_at(deadline, rpc.read(request))
            .await
            .map_err(|_| ReadError::Incomplete)?
            .map_err(status_error)?
            .into_inner();
        let challenge = match receive(&mut body, deadline).await? {
            Some(read_frame::Frame::Challenge(challenge)) => challenge,
            Some(read_frame::Frame::End(end)) => {
                return Err(early_end(&mut body, deadline, end).await);
            }
            _ => return Err(ReadError::Incomplete),
        };
        let Challenge { producer, nonce } = &challenge;
        if producer.as_slice() != self.producer || nonce.len() != 32 {
            return Err(ReadError::InvalidData("unrecognized producer or challenge"));
        }
        let proof = key.signing_key().sign(&plan.transcript(&challenge));
        timeout_at(
            deadline,
            send.send(ReadInput {
                frame: Some(read_input::Frame::Signature(proof)),
            }),
        )
        .await
        .map_err(|_| ReadError::Incomplete)?
        .map_err(|_| ReadError::Incomplete)?;
        drop(send);
        let start = match receive(&mut body, deadline).await? {
            Some(read_frame::Frame::Start(start)) => start,
            Some(read_frame::Frame::End(end)) => {
                return Err(early_end(&mut body, deadline, end).await);
            }
            _ => return Err(ReadError::Incomplete),
        };
        let at = wire::cut(start.at.as_ref())?;
        if start != plan.wire_context(self.producer, &at) {
            return Err(ReadError::InvalidData("result does not match request"));
        }
        Ok(Flow {
            body: Some(body),
            start,
            deadline,
            max_chunk: self.limits.chunk,
            delivered: false,
        })
    }
}

fn array(bytes: Vec<u8>) -> Result<[u8; 16], ReadError> {
    bytes
        .try_into()
        .map_err(|_| ReadError::InvalidData("bad producer identity"))
}

fn status_error(status: Status) -> ReadError {
    match status.code() {
        Code::ResourceExhausted => ReadError::Limit,
        Code::Unimplemented => ReadError::Unsupported,
        Code::PermissionDenied | Code::Unauthenticated => ReadError::Denied,
        Code::InvalidArgument => ReadError::InvalidData("invalid request or frame"),
        _ => ReadError::Incomplete,
    }
}

async fn receive(
    body: &mut Streaming<ReadFrame>,
    deadline: Instant,
) -> Result<Option<read_frame::Frame>, ReadError> {
    timeout_at(deadline, body.message())
        .await
        .map_err(|_| ReadError::Incomplete)?
        .map_err(status_error)?
        .map(|m| m.frame.ok_or(ReadError::InvalidData("unknown frame")))
        .transpose()
}

async fn clean_end(body: &mut Streaming<ReadFrame>, deadline: Instant) -> Result<(), ReadError> {
    match receive(body, deadline).await {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(ReadError::InvalidData("frame after terminal")),
        Err(_) => Err(ReadError::Incomplete),
    }
}

fn failure(end: End) -> ReadError {
    if end.context.is_some() {
        return ReadError::InvalidData("failure with success context");
    }
    match Outcome::try_from(end.outcome) {
        Ok(Outcome::Refused) => ReadError::Refused,
        Ok(Outcome::Denied) => ReadError::Denied,
        Ok(Outcome::InvalidBoundary) => ReadError::InvalidBoundary,
        Ok(Outcome::UnavailableHistory) => ReadError::UnavailableHistory,
        Ok(Outcome::Unsupported) => ReadError::Unsupported,
        Ok(Outcome::Limit) => ReadError::Limit,
        _ => ReadError::InvalidData("missing Start or unknown terminal outcome"),
    }
}

async fn early_end(body: &mut Streaming<ReadFrame>, deadline: Instant, end: End) -> ReadError {
    clean_end(body, deadline)
        .await
        .err()
        .unwrap_or_else(|| failure(end))
}

struct Flow {
    body: Option<Streaming<ReadFrame>>,
    start: Context,
    deadline: Instant,
    max_chunk: usize,
    delivered: bool,
}

impl Flow {
    fn source(&self) -> Result<Source, ReadError> {
        Ok(Source {
            producer: array(self.start.producer.clone())?,
            at: crate::state::ReadContext::at(
                wire::id(&self.start.database)?,
                wire::cut(self.start.at.as_ref())?,
                crate::state::ReadScope::Verified,
            ),
        })
    }

    async fn next(&mut self) -> Result<Option<Bytes>, ReadError> {
        if self.delivered {
            return Ok(None);
        }
        let mut body = self.body.take().ok_or(ReadError::Incomplete)?;
        match receive(&mut body, self.deadline).await? {
            Some(read_frame::Frame::Data(data)) => {
                if data.is_empty() || data.len() > self.max_chunk {
                    return Err(ReadError::Limit);
                }
                self.body = Some(body);
                Ok(Some(data))
            }
            Some(read_frame::Frame::End(end)) => {
                clean_end(&mut body, self.deadline).await?;
                if end.outcome != i32::from(Outcome::Complete) {
                    return Err(failure(end));
                }
                if end.context.as_ref() != Some(&self.start) {
                    return Err(ReadError::InvalidData("terminal context mismatch"));
                }
                self.delivered = true;
                Ok(None)
            }
            None => Err(ReadError::Incomplete),
            _ => Err(ReadError::InvalidData("unexpected read frame")),
        }
    }

    fn finish(&self) -> Result<(), ReadError> {
        if self.delivered {
            Ok(())
        } else {
            Err(ReadError::Incomplete)
        }
    }
}

/// A bounded stream of provisional encoded Doc bytes. Drop to cancel; no
/// background task will drain it or report completion after an early stop.
pub struct StateStream {
    flow: Flow,
    context: Projection,
    remaining: usize,
    has_data: bool,
}

impl StateStream {
    pub fn context(&self) -> &Projection {
        &self.context
    }

    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ReadError> {
        let Some(data) = self.flow.next().await? else {
            if !self.has_data {
                self.flow.delivered = false;
                return Err(ReadError::InvalidData("missing Doc body"));
            }
            return Ok(None);
        };
        self.remaining = self.remaining.checked_sub(data.len()).ok_or_else(|| {
            self.flow.body = None;
            ReadError::Limit
        })?;
        self.has_data = true;
        Ok(Some(data))
    }

    /// Report completion after `next_chunk` has returned `Ok(None)`.
    /// Does not consume more data on the caller's behalf.
    pub fn finish(self) -> Result<Projection, ReadError> {
        self.flow.finish()?;
        Ok(self.context)
    }

    /// Collect and decode the projection. No partial Doc escapes on refusal,
    /// invalid encoding or transport loss. Call this on a fresh stream.
    pub async fn collect_complete(mut self) -> Result<Delivered<Projection, Doc>, ReadError> {
        if self.has_data || self.flow.delivered {
            return Err(ReadError::Incomplete);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        let value = Doc::decode(&bytes).map_err(|_| ReadError::InvalidData("invalid Doc body"))?;
        let context = self.finish()?;
        Ok(Delivered { context, value })
    }
}

/// A bounded stream of canonical owned Entries. Delivery is not signature or
/// authority verification by the client. Preserve the distinction before
/// applying candidates or advancing a durable coverage checkpoint.
pub struct HistoryStream {
    flow: Flow,
    context: Source,
    chunk: Bytes,
    header_read: bool,
    limits: Limits,
    remaining: usize,
    needed: HashSet<ID>,
    seen: HashSet<ID>,
}

impl HistoryStream {
    pub fn context(&self) -> &Source {
        &self.context
    }

    pub async fn next_entry(&mut self) -> Result<Option<Entry>, ReadError> {
        let result = self.decode_next().await;
        if result.is_err() {
            self.flow.body = None;
            self.flow.delivered = false;
        }
        result
    }

    async fn decode_next(&mut self) -> Result<Option<Entry>, ReadError> {
        if !self.header_read {
            let header = self
                .section(car::MAX_HEADER_BYTES)
                .await?
                .ok_or(ReadError::Incomplete)?;
            car::check_header(&header, self.context.at.snapshot())?;
            self.header_read = true;
        }
        let max = car::max_section(self.limits.object).min(self.remaining);
        let Some(bytes) = self.section(max).await? else {
            if self.needed.is_empty() && self.seen.contains(self.context.database()) {
                return Ok(None);
            }
            return Err(ReadError::Incomplete);
        };
        if self.seen.len() >= self.limits.entries {
            return Err(ReadError::Limit);
        }
        self.remaining -= bytes.len();
        let entry = car::decode_entry(bytes, self.limits.object)?;
        let id = entry.id();
        if !entry.in_tree(self.context.database())
            || !self.needed.remove(&id)
            || !self.seen.insert(id)
        {
            return Err(ReadError::InvalidData(
                "foreign, duplicate or unreferenced Entry",
            ));
        }
        for parent in entry
            .parents()
            .map_err(|_| ReadError::InvalidData("invalid Entry parents"))?
        {
            if !self.seen.contains(&parent) {
                self.needed.insert(parent);
                if self.needed.len() > self.limits.entries {
                    return Err(ReadError::Limit);
                }
            }
        }
        Ok(Some(entry))
    }

    /// Read one unsigned-varint-length CAR section, even if the varint or
    /// section is split across messages. Apply the cap before allocation.
    async fn section(&mut self, max: usize) -> Result<Option<Vec<u8>>, ReadError> {
        let mut prefix = unsigned_varint::encode::usize_buffer();
        let mut length = None;
        for (i, slot) in prefix.iter_mut().enumerate() {
            if self.chunk.is_empty() {
                self.chunk = match self.flow.next().await? {
                    Some(chunk) => chunk,
                    None if i == 0 => return Ok(None),
                    None => return Err(ReadError::Incomplete),
                };
            }
            let byte = self.chunk.get_u8();
            *slot = byte;
            if byte < 0x80 {
                let (n, _) = unsigned_varint::decode::usize(&prefix[..=i])
                    .map_err(|_| ReadError::InvalidData("invalid CAR length"))?;
                length = Some(n);
                break;
            }
        }
        let length = length.ok_or(ReadError::InvalidData("unterminated CAR length"))?;
        if length == 0 {
            return Err(ReadError::InvalidData("empty CAR section"));
        }
        if length > max {
            return Err(ReadError::Limit);
        }
        let mut result = Vec::new();
        result
            .try_reserve_exact(length)
            .map_err(|_| ReadError::Limit)?;
        while result.len() != length {
            if self.chunk.is_empty() {
                self.chunk = self.flow.next().await?.ok_or(ReadError::Incomplete)?;
            }
            let n = self.chunk.len().min(length - result.len());
            result.extend_from_slice(&self.chunk.split_to(n));
        }
        Ok(Some(result))
    }

    /// Report complete delivery only after `next_entry` has returned `Ok(None)`.
    pub fn finish(self) -> Result<Source, ReadError> {
        self.flow.finish()?;
        Ok(self.context)
    }

    /// Explicitly collect the full admitted history (not a constant-memory helper).
    pub async fn collect_complete(mut self) -> Result<Delivered<Source, Vec<Entry>>, ReadError> {
        if self.header_read {
            return Err(ReadError::Incomplete);
        }
        let mut value = Vec::new();
        while let Some(entry) = self.next_entry().await? {
            value.push(entry);
        }
        let context = self.finish()?;
        Ok(Delivered { context, value })
    }
}
