//! Opt-in, installation-local storage for the instance device key.
//!
//! Unlike a cache, this file is authoritative. Only strict initialisation may
//! publish it; opening an instance only reads it.

use std::path::{Path, PathBuf};

use super::InstanceError;
use crate::{
    Result,
    backend::{BackendImpl, InstanceSecrets},
};

pub(super) fn failure(reason: impl Into<String>) -> crate::Error {
    InstanceError::CredentialStore {
        reason: reason.into(),
    }
    .into()
}

pub(super) async fn reject_legacy(backend: &dyn BackendImpl) -> Result<()> {
    match backend.get_instance_secrets().await {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(failure(
            "legacy instance credentials are present; migration is not supported",
        )),
        // Backend parser errors may include the legacy contents.
        Err(_) => Err(failure(
            "could not verify absence of legacy instance credentials",
        )),
    }
}

pub(super) async fn load(directory: &Path) -> Result<InstanceSecrets> {
    // Drop trailing path separators so they cannot bypass O_NOFOLLOW.
    let directory = directory.components().collect::<PathBuf>();
    run_io(move || platform::load(&directory)).await
}

pub(super) async fn create(directory: &Path, secrets: InstanceSecrets) -> Result<()> {
    let directory = directory.components().collect::<PathBuf>();
    run_io(move || platform::create(&directory, &secrets)).await
}

async fn run_io<T: Send + 'static>(op: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(op)
        .await
        .map_err(|_| failure("credential store I/O task did not finish"))?
}

#[cfg(unix)]
mod platform {
    use std::{
        fs::{File, Metadata, OpenOptions},
        io::{self, Read},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };

    use zeroize::Zeroizing;

    use super::{InstanceSecrets, Path, Result, failure};

    const FILE_NAME: &str = "instance-secrets.json";
    const MAX_SIZE: u64 = 1024;

    fn io_error(action: &str, error: io::Error) -> crate::Error {
        failure(format!("{action}: {error}"))
    }

    fn owned_with_mode(metadata: &Metadata, mode: u32) -> bool {
        // SAFETY: geteuid has no preconditions or pointers.
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o7777 == mode
    }

    fn open_directory(path: &Path) -> Result<File> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| io_error("cannot open credential directory", e))?;
        let metadata = dir
            .metadata()
            .map_err(|e| io_error("cannot inspect credential directory", e))?;
        if !owned_with_mode(&metadata, 0o700) {
            return Err(failure(
                "credential directory must be owned by this user and have mode 0700",
            ));
        }
        Ok(dir)
    }

    fn check_file(file: &File) -> Result<()> {
        let metadata = file
            .metadata()
            .map_err(|e| io_error("cannot inspect credential file", e))?;
        if !metadata.is_file() || !owned_with_mode(&metadata, 0o600) {
            return Err(failure(
                "credential file must be a regular file owned by this user with mode 0600",
            ));
        }
        Ok(())
    }

    pub(super) fn load(directory: &Path) -> Result<InstanceSecrets> {
        open_directory(directory)?;
        let file = OpenOptions::new()
            .read(true)
            // Refuse symlinks, and don't block opening a FIFO before fstat.
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join(FILE_NAME))
            .map_err(|e| io_error("cannot open existing credential file", e))?;
        check_file(&file)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SIZE as usize + 1));
        file.take(MAX_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("cannot read credential file", e))?;
        if bytes.len() as u64 > MAX_SIZE {
            return Err(failure("credential file is too large"));
        }
        // A parse error can quote the input. Never return it to a caller or log.
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid credential file"))
    }

    pub(super) fn create(directory: &Path, secrets: &InstanceSecrets) -> Result<()> {
        let dir = open_directory(directory)?;
        let staging = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o600))
            .tempfile_in(directory)
            .map_err(|e| io_error("cannot stage credential file", e))?;
        check_file(staging.as_file())?;
        serde_json::to_writer(staging.as_file(), secrets)
            .map_err(|_| failure("cannot write credential file"))?;
        staging
            .as_file()
            .sync_all()
            .map_err(|e| io_error("cannot sync credential file", e))?;
        staging
            .persist_noclobber(directory.join(FILE_NAME))
            .map_err(|e| {
                if e.error.kind() == io::ErrorKind::AlreadyExists {
                    failure("credential file already exists; refusing to replace it")
                } else {
                    io_error("cannot publish credential file", e.error)
                }
            })?;
        // Credentials must be durable before the history publishes an instance
        // identity. An uncertain write leaves the file in place for inspection.
        dir.sync_all()
            .map_err(|e| io_error("cannot sync credential directory", e))
    }
}

#[cfg(not(unix))]
mod platform {
    use super::{InstanceSecrets, Path, Result, failure};

    pub(super) fn load(_directory: &Path) -> Result<InstanceSecrets> {
        Err(failure("the separate credential store requires Unix"))
    }

    pub(super) fn create(_directory: &Path, _secrets: &InstanceSecrets) -> Result<()> {
        Err(failure("the separate credential store requires Unix"))
    }
}
