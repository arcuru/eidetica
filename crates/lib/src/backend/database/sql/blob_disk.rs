//! On-disk tier for the hybrid blob store (§5.2).
//!
//! Large blobs are stored as content-addressed files on disk instead of inline
//! in the SQL `data` column: a multi-GB cell is its own problem (no `mmap`, no
//! `sendfile`, and "orders of magnitude slower" to serve — exactly iroh's
//! rejected pure-database approach). The SQL `blobs` row still holds the
//! metadata (`size`, `outboard`, `last_accessed`, `location = 1`, `data` NULL);
//! only the bytes live here, in a file named for the blob's CID.
//!
//! This whole module is §4.3 throwaway internals: the on-disk path layout is
//! not observable to callers or peers, so it can change freely. The CID string
//! is base32lower (filesystem-safe), so the file name is just the CID.
//!
//! Reads are `pread`-style — a range read opens the file, seeks to the window,
//! and reads only those bytes — so a verified range serve of a large on-disk
//! blob never materializes the whole file. Writes are atomic: a temp file on
//! the same directory (hence same device) is written, fsync'd, then renamed
//! into place, so a crash mid-write never leaves a torn blob under its CID.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::Result;
use crate::backend::errors::BackendError;
use crate::entry::ID;

/// Map an I/O failure to a backend error, preserving the source.
fn io_err(source: std::io::Error) -> crate::Error {
    BackendError::FileIo { source }.into()
}

/// The on-disk path for a blob: `<dir>/<cid>`. The CID is base32lower, so it is
/// already a safe single-segment file name (no separators, no traversal).
fn blob_path(dir: &Path, cid: &ID) -> PathBuf {
    dir.join(cid.to_string())
}

/// Write a blob's bytes to its content-addressed file atomically.
///
/// Existence-check first: a present file already holds the identical bytes
/// (content addressing), and creating file metadata dwarfs writing a few bytes
/// (iroh's note), so re-storing is a cheap no-op. Otherwise write to a unique
/// temp file in the same directory, fsync it, and rename into place — the
/// rename is atomic on a single device, which a same-directory temp guarantees.
pub async fn write_atomic(dir: &Path, cid: &ID, data: &[u8]) -> Result<()> {
    let final_path = blob_path(dir, cid);
    // Cheap idempotency / dedup: the file is named by content, so if it exists
    // it is already exactly these bytes.
    if tokio::fs::try_exists(&final_path).await.map_err(io_err)? {
        return Ok(());
    }

    tokio::fs::create_dir_all(dir).await.map_err(io_err)?;

    // Unique temp name so concurrent writers of the same (or different) blobs
    // never collide on the staging file. Same directory => same device => the
    // final rename is atomic.
    let tmp_path = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));

    // Scope the file handle so it is closed before the rename.
    {
        let mut file = tokio::fs::File::create(&tmp_path).await.map_err(io_err)?;
        file.write_all(data).await.map_err(io_err)?;
        // Durably flush the bytes before the rename so a crash can't leave a
        // renamed-but-empty file under the CID.
        file.sync_all().await.map_err(io_err)?;
    }

    // Atomic publish. If a concurrent writer beat us to it, the rename still
    // yields the correct content (identical bytes), so we don't treat an
    // already-present final path as an error.
    match tokio::fs::rename(&tmp_path, &final_path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup of our temp file; ignore failure.
            let _ = tokio::fs::remove_file(&tmp_path).await;
            Err(io_err(e))
        }
    }
}

/// Read a blob's whole bytes from disk, or `None` if the file is absent.
pub async fn read_whole(dir: &Path, cid: &ID) -> Result<Option<Vec<u8>>> {
    match tokio::fs::read(blob_path(dir, cid)).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(e)),
    }
}

/// Read a byte range of an on-disk blob via `pread` (seek + read), reading only
/// the requested window — never the whole file. `range` is clamped to the file
/// length: an over-long `end` yields the available tail and an empty/past-end
/// range yields empty bytes. Returns `None` if the file is absent.
pub async fn read_range(
    dir: &Path,
    cid: &ID,
    range: std::ops::Range<u64>,
) -> Result<Option<Vec<u8>>> {
    let mut file = match tokio::fs::File::open(blob_path(dir, cid)).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };

    let len = file.metadata().await.map_err(io_err)?.len();
    let start = range.start.min(len);
    let end = range.end.clamp(start, len);
    let n = (end - start) as usize;
    if n == 0 {
        return Ok(Some(Vec::new()));
    }

    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(io_err)?;
    let mut buf = vec![0u8; n];
    // `n` is bounded by the file length, so the window is fully readable.
    file.read_exact(&mut buf).await.map_err(io_err)?;
    Ok(Some(buf))
}

/// Delete a blob's on-disk file. Absent is success (the goal state is reached).
pub async fn delete(dir: &Path, cid: &ID) -> Result<()> {
    match tokio::fs::remove_file(blob_path(dir, cid)).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(e)),
    }
}
