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
        fs::{File, Metadata},
        io::{self, Read},
        os::unix::fs::MetadataExt,
    };

    use rustix::{
        fs::{AtFlags, Mode, OFlags, linkat, open, openat, unlinkat},
        io::Errno,
        process::geteuid,
    };
    use zeroize::Zeroizing;

    use super::{InstanceSecrets, Path, Result, failure};

    const FILE_NAME: &str = "instance-secrets.json";
    const MAX_SIZE: u64 = 1024;

    fn io_error(action: &str, error: impl Into<io::Error>) -> crate::Error {
        failure(format!("{action}: {}", error.into()))
    }

    fn owned_with_mode(metadata: &Metadata, mode: u32) -> bool {
        metadata.uid() == geteuid().as_raw() && metadata.mode() & 0o7777 == mode
    }

    fn open_directory(path: &Path) -> Result<File> {
        let dir = File::from(
            open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error("cannot open credential directory", e))?,
        );
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
        let dir = open_directory(directory)?;
        let file = File::from(
            openat(
                &dir,
                FILE_NAME,
                // Don't block opening a FIFO before the regular-file check.
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error("cannot open existing credential file", e))?,
        );
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
        let name = format!(".device-key-{}.tmp", uuid::Uuid::new_v4());
        let staging = File::from(
            openat(
                &dir,
                name.as_str(),
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|e| io_error("cannot stage credential file", e))?,
        );
        let published = (|| {
            check_file(&staging)?;
            serde_json::to_writer(&staging, secrets)
                .map_err(|_| failure("cannot write credential file"))?;
            staging
                .sync_all()
                .map_err(|e| io_error("cannot sync credential file", e))?;
            // Link the synced file into place without replacing any existing entry.
            linkat(&dir, name.as_str(), &dir, FILE_NAME, AtFlags::empty()).map_err(|e| {
                if e == Errno::EXIST {
                    failure("credential file already exists; refusing to replace it")
                } else {
                    io_error("cannot publish credential file", e)
                }
            })
        })();
        unlinkat(&dir, name.as_str(), AtFlags::empty())
            .map_err(|e| io_error("cannot remove staged credential file", e))?;
        published?;
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
