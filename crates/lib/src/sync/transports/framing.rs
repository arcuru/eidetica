//! Version-one peer framing shared by HTTP and Iroh.
//!
//! A frame includes its four-byte length prefix in the unchanged 1 MiB bound.
//! The collector releases nothing until End **and** clean EOF. It validates
//! structure, not signatures, authority or Verified status; normal ingestion
//! must still run after collection. The retained response API uses O(total
//! entries) consumer memory, independently of bounded wire/producer pages.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{
    Entry, Result,
    auth::{Permission, crypto::PublicKey},
    backend::{
        HistoryPage, HistoryPageLimits,
        history_page::{HistoryQuery, MAX_HISTORY_PAGE_ENTRIES, MAX_HISTORY_TIPS, validate_page},
    },
    encoding::{json_encoded_len, serialize_capped},
    entry::ID,
    snapshot::Snapshot,
    sync::{
        SyncError,
        protocol::{BootstrapResponse, IncrementalResponse, SyncRequest, SyncResponse},
    },
};

/// Demand-driven response production. Dropping the stream drops its producer;
/// no detached task or unbounded page channel is involved.
pub type FrameStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Frame>> + Send>>;

/// Complete encoded frame ceiling, including the prefix, for both transports.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Distinct from the legacy unframed JSON protocol.
pub const WIRE_VERSION: u32 = 1;
const JSON_CAP: usize = MAX_FRAME_BYTES - 4;

/// Bound the live peer collectors/producers independently of the native
/// per-page permit, which must be released before network backpressure waits.
pub(crate) fn admit_operation() -> Result<tokio::sync::OwnedSemaphorePermit> {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    PERMITS
        .get_or_init(|| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(
                crate::backend::history_page::MAX_CONCURRENT_HISTORY_PAGES,
            ))
        })
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid("concurrent peer operations"))
}

pub(crate) fn admit_outbound() -> Result<tokio::sync::OwnedSemaphorePermit> {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    PERMITS
        .get_or_init(|| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(
                crate::backend::history_page::MAX_CONCURRENT_HISTORY_PAGES,
            ))
        })
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid("concurrent outbound peer operations"))
}

/// Encode a bounded failure even after pages have been emitted. No partial
/// sequence can become a successful logical response after this envelope.
pub fn error_frame(error: crate::Error) -> Frame {
    if let crate::Error::Backend(error) = &error {
        match &**error {
            crate::backend::BackendError::EntryTooLarge { id, encoded_bytes } => {
                return Frame::Error(PeerError::EntryTooLarge {
                    id: id.clone(),
                    limit: MAX_FRAME_BYTES,
                    encoded_bytes: *encoded_bytes,
                });
            }
            crate::backend::BackendError::DatabaseHistoryPageUnsupported => {
                return Frame::Error(PeerError::Unsupported);
            }
            _ => {}
        }
    }
    let frame = Frame::Error(PeerError::Resource(error.to_string()));
    if json_encoded_len(&frame, JSON_CAP).ok().flatten().is_some() {
        frame
    } else {
        Frame::Error(PeerError::Resource("peer operation failed".into()))
    }
}

fn invalid(reason: &str) -> crate::Error {
    SyncError::SerializationError(format!("invalid peer frame sequence: {reason}")).into()
}

/// One finite authorized history operation, not a resumable bearer token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryStart {
    pub version: u32,
    pub server: PublicKey,
    pub request: [u8; 32],
    pub tree: ID,
    pub target: Snapshot,
    pub known: Snapshot,
    pub kind: HistoryKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HistoryKind {
    Bootstrap {
        key_approved: bool,
        granted_permission: Option<Permission>,
    },
    Incremental,
}

/// Internal peer wire envelopes. Entry-bearing logical responses cannot appear
/// in Control: they must pass through the complete history-sequence barrier.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    Request(SyncRequest),
    Control(SyncResponse),
    Start(HistoryStart),
    Page {
        binding: [u8; 32],
        ordinal: u64,
        page: HistoryPage,
    },
    End {
        binding: [u8; 32],
        pages: u64,
        entries: u64,
        covered: Snapshot,
    },
    Error(PeerError),
    UploadStart(UploadStart),
    UploadPage {
        binding: [u8; 32],
        offset: u64,
        entries: Vec<Entry>,
        next: Option<u64>,
    },
    UploadEnd {
        binding: [u8; 32],
        entries: u64,
    },
}

/// One Database submission in deterministic Database order. Counts and offsets
/// describe only this input list; they never assert full history coverage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadStart {
    pub version: u32,
    pub tree: ID,
    pub entries: u64,
    pub nonce: [u8; 32],
}

/// Size refusal is distinct from a transient network failure. Report only an
/// authorized Entry ID and a capped-size lower bound, never its payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PeerError {
    EntryTooLarge {
        id: ID,
        limit: usize,
        encoded_bytes: usize,
    },
    Unsupported,
    Resource(String),
}

impl PeerError {
    pub fn into_error(self) -> crate::Error {
        match self {
            Self::EntryTooLarge {
                id,
                limit,
                encoded_bytes,
            } => SyncError::EntryTooLarge {
                id,
                limit,
                encoded_bytes,
            }
            .into(),
            Self::Unsupported => SyncError::PeerProtocolUnsupported.into(),
            Self::Resource(reason) => invalid(&reason),
        }
    }
}

/// Capped counting and encoding use the same compact serializer. No giant
/// intermediate JSON allocation occurs, even for a rejected logical response.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let size = json_encoded_len(frame, JSON_CAP)?
        .ok_or_else(|| invalid("complete envelope exceeds 1 MiB"))?;
    let mut bytes = Vec::with_capacity(size + 4);
    bytes.extend_from_slice(&(size as u32).to_be_bytes());
    let (bytes, actual) = serialize_capped(frame, bytes, JSON_CAP)?
        .ok_or_else(|| invalid("complete envelope exceeds 1 MiB"))?;
    if actual != size {
        return Err(invalid("inconsistent envelope encoding"));
    }
    Ok(bytes)
}

/// Incremental decoder independent of QUIC reads and HTTP chunk boundaries.
/// Consumes at most one frame per call; a coalesced chunk is never expanded into
/// an unbounded Vec of frames. Announced lengths are checked before allocation.
#[derive(Default)]
pub struct Decoder {
    prefix: [u8; 4],
    prefix_len: usize,
    body_len: usize,
    body: Vec<u8>,
    failed: bool,
}

impl Decoder {
    pub fn next(&mut self, input: &mut &[u8]) -> Result<Option<Frame>> {
        if self.failed {
            return Err(invalid("decoder already failed"));
        }
        let result = self.decode(input);
        if result.is_err() {
            self.failed = true;
            self.body.clear();
        }
        result
    }

    fn decode(&mut self, input: &mut &[u8]) -> Result<Option<Frame>> {
        if self.prefix_len < 4 {
            let take = input.len().min(4 - self.prefix_len);
            self.prefix[self.prefix_len..self.prefix_len + take].copy_from_slice(&input[..take]);
            *input = &input[take..];
            self.prefix_len += take;
            if self.prefix_len != 4 {
                return Ok(None);
            }
            self.body_len = u32::from_be_bytes(self.prefix) as usize;
            if self.body_len == 0 || self.body_len > JSON_CAP {
                return Err(invalid("announced envelope length"));
            }
            self.body
                .try_reserve_exact(self.body_len)
                .map_err(|_| invalid("frame allocation"))?;
        }
        let take = input.len().min(self.body_len - self.body.len());
        self.body.extend_from_slice(&input[..take]);
        *input = &input[take..];
        if self.body.len() != self.body_len {
            return Ok(None);
        }
        let frame = serde_json::from_slice(&self.body)?;
        self.body.clear();
        self.body_len = 0;
        self.prefix_len = 0;
        Ok(Some(frame))
    }

    pub fn finish(self) -> Result<()> {
        if self.failed || self.prefix_len != 0 {
            return Err(invalid("truncated or failed frame"));
        }
        Ok(())
    }
}

fn digest<T: Serialize>(value: &T) -> Result<[u8; 32]> {
    // Metadata is capped independently, before creating a binding or producer.
    let (bytes, _) = serialize_capped(value, Vec::new(), JSON_CAP)?
        .ok_or_else(|| invalid("operation metadata exceeds frame cap"))?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

pub fn request_binding(request: &SyncRequest) -> Result<[u8; 32]> {
    digest(request)
}

impl HistoryStart {
    pub fn binding(&self) -> Result<[u8; 32]> {
        self.validate()?;
        digest(self)
    }

    fn validate(&self) -> Result<()> {
        if self.version != WIRE_VERSION
            || self.target.is_empty()
            || self.target.len() > MAX_HISTORY_TIPS
            || self.known.len() > MAX_HISTORY_TIPS
            || (matches!(self.kind, HistoryKind::Bootstrap { .. }) && !self.known.is_empty())
        {
            return Err(invalid("history start scope or version"));
        }
        Ok(())
    }

    /// Reserve the complete wrapper's exact maximum overhead before asking a
    /// native Database producer for a page. Cursor encoding is already counted
    /// by the backend; the wrapper must not spend that budget a second time.
    pub fn page_limits(&self, max_entries: usize, max_bytes: usize) -> Result<HistoryPageLimits> {
        let empty = HistoryPage::default();
        let frame = Frame::Page {
            binding: self.binding()?,
            ordinal: u64::MAX,
            page: empty.clone(),
        };
        let wrapper = json_encoded_len(&frame, JSON_CAP)?
            .ok_or_else(|| invalid("page wrapper metadata"))?
            - json_encoded_len(&empty, JSON_CAP)?.expect("empty page fits");
        let budget = max_bytes
            .min(JSON_CAP)
            .checked_sub(wrapper)
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("page wrapper metadata"))?;
        Ok(HistoryPageLimits {
            max_entries: max_entries.clamp(1, MAX_HISTORY_PAGE_ENTRIES),
            max_encoded_bytes: budget,
            allow_large_singleton: false,
        })
    }
}

/// Operation-private complete-response collector. Dropping it, or any failure,
/// releases the collected entries without publishing a received prefix.
pub struct Collector {
    request: SyncRequest,
    expected: [u8; 32],
    start: Option<HistoryStart>,
    binding: Option<[u8; 32]>,
    previous: Option<(u64, ID)>,
    seen: HashSet<ID>,
    covered: BTreeSet<ID>,
    entries: Vec<Entry>,
    pages: u64,
    terminal_page: bool,
    end: bool,
    failed: bool,
    control: Option<SyncResponse>,
    deadline: Instant,
}

impl Collector {
    pub fn new(request: SyncRequest, deadline: Instant) -> Result<Self> {
        let expected = if matches!(request, SyncRequest::SendEntries(_)) {
            [0; 32]
        } else {
            request_binding(&request)?
        };
        Ok(Self {
            request,
            expected,
            start: None,
            binding: None,
            previous: None,
            seen: HashSet::new(),
            covered: BTreeSet::new(),
            entries: Vec::new(),
            pages: 0,
            terminal_page: false,
            end: false,
            failed: false,
            control: None,
            deadline,
        })
    }

    pub fn push(&mut self, frame: Frame) -> Result<()> {
        if self.failed {
            return Err(invalid("collector already failed"));
        }
        let result = self.accept(frame);
        if result.is_err() {
            self.failed = true;
            self.entries.clear();
            self.control = None;
        }
        result
    }

    fn check_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(invalid("whole exchange deadline"));
        }
        Ok(())
    }

    fn accept(&mut self, frame: Frame) -> Result<()> {
        self.check_deadline()?;
        if self.end || self.control.is_some() {
            return Err(invalid("trailing operation after completion"));
        }
        match frame {
            Frame::Error(error) => return Err(error.into_error()),
            Frame::Control(response) if self.start.is_none() => {
                let matches = matches!(
                    (&self.request, &response),
                    (_, SyncResponse::Error(_))
                        | (SyncRequest::Handshake(_), SyncResponse::Handshake(_))
                        | (
                            SyncRequest::SendEntries(_),
                            SyncResponse::Ack | SyncResponse::Count(_)
                        )
                        | (
                            SyncRequest::SyncTree(_),
                            SyncResponse::BootstrapPending { .. }
                                | SyncResponse::BootstrapRejected { .. }
                        )
                );
                if !matches {
                    return Err(invalid("unexpected or entry-bearing control response"));
                }
                self.control = Some(response);
            }
            Frame::Start(start) if self.start.is_none() => {
                start.validate()?;
                let SyncRequest::SyncTree(request) = &self.request else {
                    return Err(invalid("unexpected history response"));
                };
                let bootstrap =
                    request.our_tips.is_empty() || request.requested_permission.is_some();
                if start.request != self.expected
                    || start.tree != request.tree_id
                    || bootstrap != matches!(start.kind, HistoryKind::Bootstrap { .. })
                    || (!bootstrap && start.known != request.our_tips)
                {
                    return Err(invalid("request/history binding"));
                }
                self.binding = Some(start.binding()?);
                self.covered = start
                    .target
                    .iter()
                    .filter(|id| start.known.contains(id))
                    .cloned()
                    .collect();
                self.start = Some(start);
            }
            Frame::Page {
                binding,
                ordinal,
                page,
            } => {
                let start = self
                    .start
                    .as_ref()
                    .ok_or_else(|| invalid("page before start"))?;
                if Some(binding) != self.binding
                    || ordinal != self.pages
                    || self.terminal_page
                    || page.entries.is_empty()
                    || page.entries.len() > MAX_HISTORY_PAGE_ENTRIES
                {
                    return Err(invalid("page binding, ordinal or progress"));
                }
                let query = HistoryQuery::database(
                    &start.tree,
                    &start.target,
                    &start.known,
                    None,
                    self.deadline,
                )?;
                validate_page(&query, &page, &mut self.previous)?;
                for entry in &page.entries {
                    if start.known.contains(&entry.id()) || self.seen.contains(&entry.id()) {
                        return Err(invalid("duplicate or boundary entry"));
                    }
                    for parent in entry.parents()? {
                        if !self.seen.contains(&parent) && !start.known.contains(&parent) {
                            return Err(invalid("missing main parent"));
                        }
                        self.covered.remove(&parent);
                    }
                    self.seen.insert(entry.id());
                    self.covered.insert(entry.id());
                }
                self.terminal_page = page.next.is_none();
                self.pages = self
                    .pages
                    .checked_add(1)
                    .ok_or_else(|| invalid("page counter"))?;
                self.entries.extend(page.entries);
            }
            Frame::End {
                binding,
                pages,
                entries,
                covered,
            } => {
                let start = self
                    .start
                    .as_ref()
                    .ok_or_else(|| invalid("end before start"))?;
                if Some(binding) != self.binding
                    || pages != self.pages
                    || entries != self.entries.len() as u64
                    || (self.pages != 0 && !self.terminal_page)
                    || covered != Snapshot::from(self.covered.iter().cloned().collect::<Vec<_>>())
                    || covered != start.target
                {
                    return Err(invalid("end counts, coverage or terminal page"));
                }
                self.end = true;
            }
            _ => return Err(invalid("unexpected envelope")),
        }
        Ok(())
    }

    /// Call only after the decoder has observed clean EOF. No terminal marker
    /// alone returns the logical response or permits the caller to ingest it.
    pub fn finish(self, decoder: Decoder) -> Result<SyncResponse> {
        decoder.finish()?;
        self.check_deadline()?;
        if self.failed {
            return Err(invalid("failed sequence"));
        }
        if let Some(response) = self.control {
            return Ok(response);
        }
        if !self.end {
            return Err(invalid("EOF before End"));
        }
        let start = self.start.ok_or_else(|| invalid("missing start"))?;
        match start.kind {
            HistoryKind::Bootstrap {
                key_approved,
                granted_permission,
            } => {
                let mut entries = self.entries.into_iter();
                let root = entries
                    .next()
                    .filter(|e| e.id() == start.tree && e.root().is_none())
                    .ok_or_else(|| invalid("missing bootstrap root"))?;
                Ok(SyncResponse::Bootstrap(BootstrapResponse {
                    tree_id: start.tree,
                    root_entry: root,
                    all_entries: entries.collect(),
                    key_approved,
                    granted_permission,
                }))
            }
            HistoryKind::Incremental => Ok(SyncResponse::Incremental(IncrementalResponse {
                tree_id: start.tree,
                their_tips: start.target.into_tips(),
                missing_entries: self.entries,
            })),
        }
    }
}

/// Feed one network chunk without assuming it contains exactly one frame.
pub fn receive_chunk(
    decoder: &mut Decoder,
    collector: &mut Collector,
    mut chunk: &[u8],
) -> Result<()> {
    while !chunk.is_empty() {
        if let Some(frame) = decoder.next(&mut chunk)? {
            collector.push(frame)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Serialize a borrowed request under downstream backpressure. Input-list
/// uploads retain only references and one bounded page, not a whole encoded
/// body or an additional whole-transfer Entry clone.
pub async fn write_request<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    request: &SyncRequest,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let SyncRequest::SendEntries(entries) = request else {
        writer
            .write_all(&encode(&Frame::Request(request.clone()))?)
            .await?;
        return Ok(());
    };
    if entries.is_empty() {
        writer
            .write_all(&encode(&Frame::Request(SyncRequest::SendEntries(
                Vec::new(),
            )))?)
            .await?;
        return Ok(());
    }
    let mut groups = std::collections::BTreeMap::<ID, Vec<&Entry>>::new();
    for entry in entries {
        groups
            .entry(entry.root().unwrap_or_else(|| entry.id()))
            .or_default()
            .push(entry);
    }
    for (tree, entries) in groups {
        let start = UploadStart {
            version: WIRE_VERSION,
            tree,
            entries: entries.len() as u64,
            nonce: crate::auth::crypto::generate_challenge()
                .try_into()
                .expect("32-byte challenge"),
        };
        let binding = digest(&start)?;
        writer
            .write_all(&encode(&Frame::UploadStart(start))?)
            .await?;
        let mut offset = 0;
        while offset < entries.len() {
            let empty = Frame::UploadPage {
                binding,
                offset: offset as u64,
                entries: Vec::new(),
                next: None,
            };
            let mut budget = crate::backend::record_page::PageBudget::new(
                &empty,
                MAX_HISTORY_PAGE_ENTRIES,
                JSON_CAP,
                JSON_CAP,
                false,
            )?;
            let mut selected = Vec::new();
            while offset + selected.len() < entries.len() && !budget.is_full() {
                let position = offset + selected.len();
                let entry = entries[position];
                let item = json_encoded_len(entry, budget.ceiling())?;
                let next = json_encoded_len(&(position as u64 + 1), budget.ceiling())?;
                match budget.admit(item, next, position + 1 < entries.len()) {
                    Ok(true) => selected.push(entry.clone()),
                    Ok(false) => break,
                    Err(_) => {
                        return Err(SyncError::EntryTooLarge {
                            id: entry.id(),
                            limit: MAX_FRAME_BYTES,
                            encoded_bytes: MAX_FRAME_BYTES + 1,
                        }
                        .into());
                    }
                }
            }
            let begin = offset;
            offset += selected.len();
            let frame = Frame::UploadPage {
                binding,
                offset: begin as u64,
                entries: selected,
                next: (offset < entries.len()).then_some(offset as u64),
            };
            writer.write_all(&encode(&frame)?).await?;
        }
        writer
            .write_all(&encode(&Frame::UploadEnd {
                binding,
                entries: entries.len() as u64,
            })?)
            .await?;
    }
    Ok(())
}

/// Collect a complete request body before handing any uploaded entries to the
/// existing ingestion path. A failed or cancelled body cannot publish prefixes.
pub struct RequestCollector {
    request: Option<SyncRequest>,
    active: Option<UploadStart>,
    binding: Option<[u8; 32]>,
    offset: u64,
    terminal: bool,
    last_tree: Option<ID>,
    entries: Vec<Entry>,
    failed: bool,
    deadline: Instant,
}

impl RequestCollector {
    pub fn new(deadline: Instant) -> Self {
        Self {
            request: None,
            active: None,
            binding: None,
            offset: 0,
            terminal: false,
            last_tree: None,
            entries: Vec::new(),
            failed: false,
            deadline,
        }
    }

    pub fn push(&mut self, frame: Frame) -> Result<()> {
        if self.failed {
            return Err(invalid("request collector already failed"));
        }
        let result = self.accept(frame);
        if result.is_err() {
            self.failed = true;
            self.entries.clear();
            self.request = None;
        }
        result
    }

    fn accept(&mut self, frame: Frame) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(invalid("request deadline"));
        }
        if self.request.is_some() {
            return Err(invalid("trailing request"));
        }
        match frame {
            Frame::Request(request) if self.active.is_none() && self.last_tree.is_none() => {
                if let SyncRequest::SendEntries(entries) = &request
                    && !entries.is_empty()
                {
                    return Err(invalid("entry-bearing unpaged request"));
                }
                self.request = Some(request);
            }
            Frame::UploadStart(start) if self.active.is_none() => {
                if start.version != WIRE_VERSION
                    || start.entries == 0
                    || self
                        .last_tree
                        .as_ref()
                        .is_some_and(|last| start.tree <= *last)
                {
                    return Err(invalid("submission version, count or Database order"));
                }
                self.binding = Some(digest(&start)?);
                self.offset = 0;
                self.terminal = false;
                self.active = Some(start);
            }
            Frame::UploadPage {
                binding,
                offset,
                entries,
                next,
            } => {
                let start = self
                    .active
                    .as_ref()
                    .ok_or_else(|| invalid("upload page before start"))?;
                let end = self
                    .offset
                    .checked_add(entries.len() as u64)
                    .ok_or_else(|| invalid("upload offset"))?;
                if Some(binding) != self.binding
                    || offset != self.offset
                    || self.terminal
                    || entries.is_empty()
                    || entries.len() > MAX_HISTORY_PAGE_ENTRIES
                    || end > start.entries
                    || next != (end < start.entries).then_some(end)
                    || entries.iter().any(|entry| !entry.in_tree(&start.tree))
                {
                    return Err(invalid("upload scope, offset, count or progress"));
                }
                self.offset = end;
                self.terminal = next.is_none();
                self.entries.extend(entries);
            }
            Frame::UploadEnd { binding, entries } => {
                let start = self
                    .active
                    .as_ref()
                    .ok_or_else(|| invalid("upload end before start"))?;
                if Some(binding) != self.binding
                    || !self.terminal
                    || entries != self.offset
                    || entries != start.entries
                {
                    return Err(invalid("incomplete submission"));
                }
                self.last_tree = self.active.take().map(|start| start.tree);
                self.binding = None;
            }
            _ => return Err(invalid("unexpected request envelope")),
        }
        Ok(())
    }

    pub fn finish(self, decoder: Decoder) -> Result<SyncRequest> {
        decoder.finish()?;
        if self.failed || Instant::now() >= self.deadline || self.active.is_some() {
            return Err(invalid("incomplete or expired request"));
        }
        if let Some(request) = self.request {
            return Ok(request);
        }
        if self.last_tree.is_none() {
            return Err(invalid("empty request"));
        }
        Ok(SyncRequest::SendEntries(self.entries))
    }
}

pub fn receive_request_chunk(
    decoder: &mut Decoder,
    collector: &mut RequestCollector,
    mut chunk: &[u8],
) -> Result<()> {
    while !chunk.is_empty() {
        if let Some(frame) = decoder.next(&mut chunk)? {
            collector.push(frame)?;
        }
    }
    Ok(())
}

impl Collector {
    pub fn for_request(request: &SyncRequest, deadline: Instant) -> Result<Self> {
        Self::new(
            match request {
                SyncRequest::SendEntries(_) => SyncRequest::SendEntries(Vec::new()),
                _ => request.clone(),
            },
            deadline,
        )
    }
}

pub async fn read_request<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    deadline: Instant,
) -> Result<SyncRequest> {
    use tokio::io::AsyncReadExt;
    let _permit = admit_operation()?;
    let mut decoder = Decoder::default();
    let mut collector = RequestCollector::new(deadline);
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            return collector.finish(decoder);
        }
        receive_request_chunk(&mut decoder, &mut collector, &buffer[..n])?;
    }
}

pub async fn read_response<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    request: &SyncRequest,
    deadline: Instant,
) -> Result<SyncResponse> {
    use tokio::io::AsyncReadExt;
    let mut decoder = Decoder::default();
    let mut collector = Collector::for_request(request, deadline)?;
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            return collector.finish(decoder);
        }
        receive_chunk(&mut decoder, &mut collector, &buffer[..n])?;
    }
}

/// Prepare a demand-driven byte stream with the same absolute producer
/// deadline as its transport. Body drop cancels production; no task outlives it.
pub async fn response_bytes(
    handler: std::sync::Arc<dyn crate::sync::handler::SyncHandler>,
    request: SyncRequest,
    remote_address: crate::sync::peer_types::Address,
    deadline: Instant,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Vec<u8>>> + Send>> {
    use futures_util::StreamExt;
    let peer_pubkey = match &request {
        SyncRequest::SyncTree(request) => request.peer_pubkey.clone(),
        _ => None,
    };
    let context = crate::sync::protocol::RequestContext {
        remote_address: Some(remote_address),
        peer_pubkey,
    };
    let frames = match tokio::time::timeout_at(
        deadline,
        handler.response_frames(request, context, deadline),
    )
    .await
    {
        Ok(Ok(frames)) => frames,
        result => {
            let error = match result {
                Ok(Err(error)) => error,
                _ => invalid("producer deadline"),
            };
            Box::pin(futures_util::stream::iter([Ok(error_frame(error))])) as FrameStream
        }
    };
    Box::pin(futures_util::stream::unfold(
        (frames, false),
        move |(mut frames, done)| async move {
            if done {
                return None;
            }
            let (frame, terminal) = match tokio::time::timeout_at(deadline, frames.next()).await {
                Ok(Some(Ok(frame))) => (frame, false),
                Ok(Some(Err(error))) => (error_frame(error), true),
                Ok(None) => return None,
                Err(_) => (error_frame(invalid("producer deadline")), true),
            };
            let encoded = encode(&frame).or_else(|error| encode(&error_frame(error)));
            Some((encoded, (frames, terminal)))
        },
    ))
}
