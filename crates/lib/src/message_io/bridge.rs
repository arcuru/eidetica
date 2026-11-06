//! The join between async native streams and synchronous codec workers.
//!
//! Sockets and bodies stay on the async side; encoding and decoding run on a
//! blocking worker. A bounded channel of fixed-size chunks connects the two, so
//! a stalled peer stalls the worker rather than growing a buffer. Dropping the
//! async side cancels the worker, and a worker keeps its admission [`Lease`]
//! until it has really stopped.

use std::{
    io::{self, BufRead, Read, Write},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt, stream};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    runtime::Handle,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

use super::{
    Lease, ProgressMonitor, ResourceLimit, TransferBudget, TransferError, TransferPolicy,
    codec::{CHUNK_BYTES, CountingWriter, INLINE_MESSAGE_BYTES, decode, encode_inline},
};

/// Bytes in the envelope's length declaration.
const PREFIX_BYTES: usize = 8;
/// Chunks queued between a worker and its stream, per direction.
const QUEUE_CHUNKS: usize = 2;

/// Cancels a worker when dropped, including by a cancelled future. Dropping a
/// `JoinHandle` alone would leave the worker running.
///
/// A worker reports cancellation as `ConnectionAborted`, never `Interrupted`,
/// which `write_all` would retry forever.
#[derive(Clone)]
struct Cancellation(Arc<watch::Sender<bool>>);

impl Cancellation {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    fn cancel(&self) {
        self.0.send_replace(true);
    }

    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    async fn cancelled(&self) {
        let mut flag = self.0.subscribe();
        // The sender is held by `self`, so the wait cannot fail.
        let _ = flag.wait_for(|cancelled| *cancelled).await;
    }
}

struct CancelOnDrop(Cancellation);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// A decoded message and the admission it still holds.
///
/// Inline messages hold byte admission without a worker slot. Drop the lease
/// when the value is handed to its next owner or no longer needed.
pub(crate) struct Inbound<T> {
    pub(crate) value: T,
    pub(crate) lease: Option<Lease>,
}

/// How [`read_message`] treats the stream around the message.
#[derive(Default)]
pub(crate) struct ReadOptions<'a> {
    /// EOF before any byte is a clean end, as between messages on a socket.
    pub(crate) allow_clean_eof: bool,
    /// The stream must end exactly where the message ends.
    pub(crate) require_end: bool,
    /// Total stream length the transport declared, if it declared one.
    pub(crate) declared_stream_bytes: Option<u64>,
    /// Fail if no byte arrives within the policy's first-response wait.
    pub(crate) first_byte_timeout: bool,
    /// Upload monitor whose first-response wait this message ends.
    pub(crate) response_to: Option<&'a ProgressMonitor>,
}

/// Read exactly one message from `reader`.
///
/// Returns `None` only for a clean EOF the options allow. Nothing is returned
/// until the whole message, and any required stream end, has been accepted. On
/// a persistent stream any error leaves it unusable and the caller retires it.
pub(crate) async fn read_message<R, T>(
    reader: &mut R,
    budget: &TransferBudget,
    options: ReadOptions<'_>,
) -> Result<Option<Inbound<T>>, TransferError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned + Send + 'static,
{
    let policy = budget.policy();
    let mut prefix = [0u8; PREFIX_BYTES];
    let first = reader.read(&mut prefix[..1]);
    let received = if options.first_byte_timeout {
        tokio::time::timeout(policy.first_response_timeout, first)
            .await
            .map_err(|_| TransferError::ResponseTimeout {
                elapsed: policy.first_response_timeout,
            })??
    } else {
        first.await?
    };
    if received == 0 {
        return if options.allow_clean_eof {
            Ok(None)
        } else {
            Err(TransferError::Truncated)
        };
    }
    if let Some(upload) = options.response_to {
        upload.response_started();
    }

    let monitor = ProgressMonitor::new(policy);
    let body = async {
        monitor.record(1)?;
        read_exact_counted(reader, &mut prefix[1..], &monitor).await?;
        let len = u64::from_be_bytes(prefix);
        check_declared_length(len, policy, options.declared_stream_bytes)?;

        if len <= INLINE_MESSAGE_BYTES as u64 {
            let lease = budget.try_inline(len)?;
            let mut message = vec![0u8; len as usize];
            read_exact_counted(reader, &mut message, &monitor).await?;
            if options.require_end {
                require_end(reader, &monitor).await?;
            }
            let value = decode(message.as_slice(), policy)?;
            return Ok(Inbound {
                value,
                lease: Some(lease),
            });
        }

        let mut lease = budget.try_inbound()?;
        lease.reserve(len)?;
        decode_streamed(reader, len, lease, policy, &monitor, options.require_end).await
    };
    monitor.run(body).await.map(Some)
}

fn check_declared_length(
    len: u64,
    policy: &TransferPolicy,
    declared_stream_bytes: Option<u64>,
) -> Result<(), TransferError> {
    if len == 0 {
        return Err(TransferError::Malformed("empty message".to_string()));
    }
    if len > policy.max_message_bytes {
        return Err(TransferError::resource(
            ResourceLimit::MessageBytes,
            format!("{len} bytes exceeds {}", policy.max_message_bytes),
        ));
    }
    let expected = len.checked_add(PREFIX_BYTES as u64);
    if declared_stream_bytes.is_some_and(|declared| Some(declared) != expected) {
        return Err(TransferError::Malformed(
            "stream length disagrees with the message envelope".to_string(),
        ));
    }
    Ok(())
}

async fn read_exact_counted<R: AsyncRead + Unpin>(
    reader: &mut R,
    mut buffer: &mut [u8],
    monitor: &ProgressMonitor,
) -> Result<(), TransferError> {
    while !buffer.is_empty() {
        let received = reader.read(buffer).await?;
        if received == 0 {
            return Err(TransferError::Truncated);
        }
        monitor.record(received)?;
        buffer = &mut buffer[received..];
    }
    Ok(())
}

async fn require_end<R: AsyncRead + Unpin>(
    reader: &mut R,
    monitor: &ProgressMonitor,
) -> Result<(), TransferError> {
    let mut extra = [0u8; 1];
    if reader.read(&mut extra).await? != 0 {
        return Err(TransferError::Malformed(
            "bytes follow the message".to_string(),
        ));
    }
    monitor.body_complete(false);
    Ok(())
}

/// Feed a decoder on a worker from the stream, exactly `len` bytes.
async fn decode_streamed<R, T>(
    reader: &mut R,
    len: u64,
    lease: Lease,
    policy: &TransferPolicy,
    monitor: &ProgressMonitor,
    end_required: bool,
) -> Result<Inbound<T>, TransferError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned + Send + 'static,
{
    let (chunks, receiver) = mpsc::channel::<Vec<u8>>(QUEUE_CHUNKS);
    let cancellation = Cancellation::new();
    let _cancel = CancelOnDrop(cancellation.clone());
    let worker_policy = policy.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let source = ChunkSource {
            receiver,
            current: Vec::new(),
            position: 0,
            cancellation,
        };
        let value = decode(source, &worker_policy)?;
        Ok::<_, TransferError>(Inbound {
            value,
            lease: Some(lease),
        })
    });

    let pumped = async {
        let mut left = len;
        while left > 0 {
            let want = left.min(CHUNK_BYTES as u64) as usize;
            let mut chunk = vec![0u8; want];
            let received = reader.read(&mut chunk).await?;
            if received == 0 {
                return Err(TransferError::Truncated);
            }
            monitor.record(received)?;
            left -= received as u64;
            chunk.truncate(received);
            if chunks.send(chunk).await.is_err() {
                // The decoder stopped early; its result says why.
                return Ok(());
            }
        }
        if end_required {
            require_end(reader, monitor).await?;
        }
        Ok(())
    }
    .await;
    drop(chunks);
    pumped?;
    // Only local decoding remains, which says nothing about the peer's progress.
    monitor.body_complete(false);
    worker.await.map_err(|_| TransferError::Cancelled)?
}

/// Blocking byte source over the chunk channel.
struct ChunkSource {
    receiver: mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
    cancellation: Cancellation,
}

impl Read for ChunkSource {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let read = available.len().min(target.len());
        target[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

impl BufRead for ChunkSource {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.position == self.current.len() {
            if self.cancellation.is_cancelled() {
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
            self.position = 0;
            self.current = self.receiver.blocking_recv().unwrap_or_default();
        }
        Ok(&self.current[self.position..])
    }

    fn consume(&mut self, consumed: usize) {
        self.position += consumed;
    }
}

/// Encoded message ready to move.
pub(crate) struct PreparedMessage {
    message_bytes: u64,
    source: Source,
    prefix_sent: bool,
}

enum Source {
    Inline {
        bytes: Option<Vec<u8>>,
        _lease: Option<Lease>,
    },
    Worker {
        chunks: mpsc::Receiver<Vec<u8>>,
        worker: Option<JoinHandle<Result<(), TransferError>>>,
        cancellation: Cancellation,
        _cancel: CancelOnDrop,
    },
}

/// Size `value` and get it ready to send, without sending anything.
///
/// A refusal here leaves the stream untouched, so a connection stays usable.
pub(crate) async fn prepare_message<T>(
    value: T,
    budget: &TransferBudget,
) -> Result<PreparedMessage, TransferError>
where
    T: Serialize + Send + 'static,
{
    prepare_admitted(value, budget, None).await
}

/// Reuse a queue reservation, retaining its accounting until encoding exits.
pub(crate) async fn prepare_admitted<T>(
    value: T,
    budget: &TransferBudget,
    lease: Option<Lease>,
) -> Result<PreparedMessage, TransferError>
where
    T: Serialize + Send + 'static,
{
    if let Some(message) = encode_inline(&value) {
        let lease = match lease {
            Some(mut lease) => {
                lease.reserve(message.len() as u64)?;
                lease
            }
            None => budget.try_inline(message.len() as u64)?,
        };
        return Ok(PreparedMessage {
            message_bytes: message.len() as u64,
            source: Source::Inline {
                bytes: Some(message),
                _lease: Some(lease),
            },
            prefix_sent: false,
        });
    }

    let lease = match lease {
        Some(lease) => lease,
        None => budget.try_outbound()?,
    };
    let policy = budget.policy();
    let (sized, size) = oneshot::channel();
    let (chunk_sender, chunks) = mpsc::channel(QUEUE_CHUNKS);
    let cancellation = Cancellation::new();
    let worker = tokio::task::spawn_blocking({
        let cancellation = cancellation.clone();
        let limit = policy.max_message_bytes;
        let handle = Handle::current();
        move || {
            encode_worker(
                value,
                lease,
                limit,
                sized,
                chunk_sender,
                handle,
                &cancellation,
            )
        }
    });
    let guard = CancelOnDrop(cancellation.clone());

    let elapsed = policy.preparation_timeout;
    let message_bytes = match tokio::time::timeout(elapsed, size).await {
        Err(_) => return Err(TransferError::PreparationTimeout { elapsed }),
        Ok(Err(_)) => {
            return Err(match worker.await {
                Ok(Err(error)) => error,
                _ => TransferError::Cancelled,
            });
        }
        Ok(Ok(bytes)) => bytes,
    };
    Ok(PreparedMessage {
        message_bytes,
        source: Source::Worker {
            chunks,
            worker: Some(worker),
            cancellation,
            _cancel: guard,
        },
        prefix_sent: false,
    })
}

/// Size `value`, then stream it. Both passes read the same owned value: the
/// length is declared before the first byte, without an encoded copy.
fn encode_worker<T: Serialize>(
    value: T,
    mut lease: Lease,
    limit: u64,
    sized: oneshot::Sender<u64>,
    chunks: mpsc::Sender<Vec<u8>>,
    handle: Handle,
    cancellation: &Cancellation,
) -> Result<(), TransferError> {
    let cancelled = || cancellation.is_cancelled();
    let mut counter = CountingWriter {
        written: 0,
        limit,
        cancelled: &cancelled,
    };
    if let Err(error) = cbor4ii::serde::to_writer(&mut counter, &value) {
        if cancelled() {
            return Err(TransferError::Cancelled);
        }
        if counter.written > limit {
            return Err(TransferError::resource(
                ResourceLimit::MessageBytes,
                format!("encoded message exceeds {limit} bytes"),
            ));
        }
        return Err(TransferError::Encode(error.to_string()));
    }
    let message_bytes = counter.written;
    lease.reserve(message_bytes)?;
    if sized.send(message_bytes).is_err() {
        return Err(TransferError::Cancelled);
    }

    let mut writer = ChunkWriter {
        buffer: Vec::with_capacity(CHUNK_BYTES),
        written: 0,
        limit: message_bytes,
        chunks,
        handle,
        cancellation,
    };
    cbor4ii::serde::to_writer(&mut writer, &value)
        .map_err(|error| TransferError::Encode(error.to_string()))?;
    writer.flush()?;
    if writer.written != message_bytes {
        return Err(TransferError::Encode(
            "encoded size changed between passes".to_string(),
        ));
    }
    Ok(())
}

/// Blocking writer splitting output into fixed chunks before they are queued.
struct ChunkWriter<'a> {
    buffer: Vec<u8>,
    written: u64,
    limit: u64,
    chunks: mpsc::Sender<Vec<u8>>,
    handle: Handle,
    cancellation: &'a Cancellation,
}

impl ChunkWriter<'_> {
    /// Queue one chunk. Waits on cancellation too: a consumer that has stopped
    /// polling, such as a stalled HTTP response, must not pin this worker.
    fn send(&mut self) -> io::Result<()> {
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_BYTES));
        let (chunks, cancellation) = (&self.chunks, self.cancellation);
        self.handle.block_on(async {
            tokio::select! {
                sent = chunks.send(chunk) => sent.map_err(|_| io::ErrorKind::BrokenPipe.into()),
                () = cancellation.cancelled() => Err(io::ErrorKind::ConnectionAborted.into()),
            }
        })
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.written = self.written.saturating_add(input.len() as u64);
        if self.written > self.limit {
            return Err(io::Error::other("encoded size changed between passes"));
        }
        let mut input_left = input;
        while !input_left.is_empty() {
            let room = CHUNK_BYTES - self.buffer.len();
            let (now, later) = input_left.split_at(room.min(input_left.len()));
            self.buffer.extend_from_slice(now);
            input_left = later;
            if self.buffer.len() == CHUNK_BYTES {
                self.send()?;
            }
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.send()
    }
}

impl PreparedMessage {
    /// Bytes this message occupies on a stream, envelope included.
    pub(crate) fn stream_bytes(&self) -> u64 {
        self.message_bytes + PREFIX_BYTES as u64
    }

    /// The next bytes to send, or `None` once the message is complete.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, TransferError> {
        if !self.prefix_sent {
            self.prefix_sent = true;
            return Ok(Some(self.message_bytes.to_be_bytes().to_vec()));
        }
        match &mut self.source {
            Source::Inline { bytes, .. } => Ok(bytes.take()),
            Source::Worker { chunks, worker, .. } => {
                if let Some(chunk) = chunks.recv().await {
                    return Ok(Some(chunk));
                }
                match worker.take() {
                    Some(worker) => worker.await.map_err(|_| TransferError::Cancelled)??,
                    None => return Err(TransferError::Cancelled),
                }
                Ok(None)
            }
        }
    }

    /// Write the whole message to `writer`.
    ///
    /// Fails with `monitor`'s verdict if the peer stops taking bytes. Any failure
    /// leaves the stream with a partial message, so the caller must retire it.
    pub(crate) async fn write_to<W>(
        mut self,
        writer: &mut W,
        monitor: &ProgressMonitor,
    ) -> Result<(), TransferError>
    where
        W: AsyncWrite + Unpin,
    {
        monitor
            .run(async {
                while let Some(chunk) = self.next_chunk().await? {
                    writer.write_all(&chunk).await?;
                    monitor.record(chunk.len())?;
                }
                writer.flush().await?;
                Ok(())
            })
            .await
    }
}

/// Body stream for HTTP, whose client or server pulls chunks.
///
/// A chunk counts as moved once the HTTP stack asks for the next one, except the
/// last: with a declared length the stack never asks past it, so the body ends
/// when that chunk is handed over. If the stack stops asking, nothing polls this
/// stream, so a watcher cancels the encoder and its admission is released.
pub(crate) fn into_body_stream(
    message: PreparedMessage,
    monitor: Arc<ProgressMonitor>,
    awaiting_response: bool,
) -> impl Stream<Item = Result<Bytes, TransferError>> + Send + 'static {
    if let Source::Worker { cancellation, .. } = &message.source {
        let (cancellation, watcher) = (cancellation.clone(), monitor.clone());
        tokio::spawn(async move {
            tokio::select! {
                _ = watcher.expired() => cancellation.cancel(),
                () = cancellation.cancelled() => {}
            }
        });
    }
    let left = message.stream_bytes();
    stream::try_unfold(
        (message, monitor, left, 0usize),
        move |(mut message, monitor, left, handed_out)| async move {
            monitor.record(handed_out)?;
            let Some(chunk) = message.next_chunk().await? else {
                return Ok(None);
            };
            let len = chunk.len();
            let left = left - len as u64;
            if left == 0 {
                monitor.record(len)?;
                monitor.body_complete(awaiting_response);
                return Ok(Some((Bytes::from(chunk), (message, monitor, left, 0))));
            }
            Ok(Some((Bytes::from(chunk), (message, monitor, left, len))))
        },
    )
}

/// Encode a small control message, independent of any exhausted data budget.
/// Callers use this for refusals, which must get through when budgets are full.
pub(crate) fn encode_control<T: Serialize>(value: &T) -> Result<PreparedMessage, TransferError> {
    let message = encode_inline(value)
        .ok_or_else(|| TransferError::Encode("control message too large".to_string()))?;
    Ok(PreparedMessage {
        message_bytes: message.len() as u64,
        source: Source::Inline {
            bytes: Some(message),
            _lease: None,
        },
        prefix_sent: false,
    })
}

/// Adapt a body chunk stream to [`AsyncRead`]. A yielded chunk is only held
/// while it drains; the body is never collected.
pub(crate) fn reader_for_stream<S, E>(stream: S) -> impl AsyncRead + Unpin
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    struct StreamReader<S> {
        stream: S,
        chunk: Bytes,
    }

    impl<S, E> AsyncRead for StreamReader<S>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::error::Error + Send + Sync + 'static,
    {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            target: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            while self.chunk.is_empty() {
                match std::task::ready!(self.stream.poll_next_unpin(cx)) {
                    Some(Ok(chunk)) => self.chunk = chunk,
                    Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                    None => return Poll::Ready(Ok(())),
                }
            }
            let take = self.chunk.len().min(target.remaining());
            let part = self.chunk.split_to(take);
            target.put_slice(&part);
            Poll::Ready(Ok(()))
        }
    }

    StreamReader {
        stream,
        chunk: Bytes::new(),
    }
}
