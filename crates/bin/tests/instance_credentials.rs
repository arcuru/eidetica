//! Exercise the opt-in store at the CLI process boundary with disposable SQLite state.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    process::Command,
    time::Duration,
};

use eidetica::backend::{BackendImpl, database::SqlxBackend};

fn command(root: &Path, data: &str) -> Command {
    let mut cmd = Command::new(
        std::env::var_os("NEXTEST_BIN_EXE_eidetica")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica").into()),
    );
    cmd.env("EIDETICA_BACKEND", "sqlite")
        .env("EIDETICA_DATA_DIR", root.join(data))
        .env("EIDETICA_CACHE_BACKEND", "inmemory")
        .env("EIDETICA_CACHE_DIR", root.join("cache"))
        .env_remove("EIDETICA_DEVICE_KEY_DIR")
        .env("RUST_LOG", "eidetica=debug");
    cmd
}

async fn has_legacy_secret(db: &Path) -> bool {
    SqlxBackend::open_sqlite_with_memory_cache(db, Default::default())
        .await
        .unwrap()
        .get_instance_secrets()
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn protected_credentials_survive_cli_restart_without_writing_history() {
    let root = tempfile::tempdir().unwrap();
    let credentials = root.path().join("credentials");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&credentials)
        .unwrap();
    let path = credentials.join("instance-secrets.json");
    let cli = || {
        let mut cmd = command(root.path(), "history");
        cmd.env("EIDETICA_DEVICE_KEY_DIR", &credentials);
        cmd
    };
    let init = cli()
        .args(["daemon", "init", "--username", "trial", "--passwordless"])
        .output()
        .unwrap();
    assert!(init.status.success(), "disposable trial init failed");
    let db = root.path().join("history/eidetica.db");
    assert!(
        !has_legacy_secret(&db).await,
        "ordinary SQL holds the device key"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved = fs::read(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    let secret = value["signing_key"].as_str().unwrap();
    assert!(!secret.is_empty());

    let init_stdout = String::from_utf8(init.stdout).unwrap();
    let id = init_stdout
        .split_once("Device ID:    ")
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap();
    let info = cli().arg("info").output().unwrap();
    assert!(info.status.success(), "CLI reopen failed");
    assert!(String::from_utf8_lossy(&info.stdout).contains(id));
    for log in [
        init_stdout.as_bytes(),
        &init.stderr,
        &info.stdout,
        &info.stderr,
    ] {
        assert!(
            !String::from_utf8_lossy(log).contains(secret),
            "device key in CLI output"
        );
    }
    assert!(!String::from_utf8_lossy(&fs::read(db).unwrap()).contains(secret));

    let stale = command(root.path(), "other-history")
        .env("EIDETICA_DEVICE_KEY_DIR", &credentials)
        .args(["daemon", "init", "--username", "trial", "--passwordless"])
        .output()
        .unwrap();
    assert!(
        !stale.status.success(),
        "init replaced an existing device key"
    );
    assert!(String::from_utf8_lossy(&stale.stderr).contains("refusing to replace it"));
    assert!(
        fs::read(&path).unwrap() == saved,
        "init changed the original device key"
    );
    assert_eq!(
        fs::read_dir(&credentials).unwrap().count(),
        1,
        "refused init left a staging file"
    );
    let other = SqlxBackend::open_sqlite_with_memory_cache(
        &root.path().join("other-history/eidetica.db"),
        Default::default(),
    )
    .await
    .unwrap();
    assert!(other.all_roots().await.unwrap().is_empty());
    assert!(other.get_instance_metadata().await.unwrap().is_none());
    assert!(other.get_instance_secrets().await.unwrap().is_none());

    let no_flag = command(root.path(), "history")
        .arg("info")
        .output()
        .unwrap();
    assert!(
        !no_flag.status.success(),
        "reopened trial without credentials option"
    );

    fs::remove_file(&path).unwrap();
    let socket = root.path().join("service.sock");
    let mut daemon = tokio::process::Command::from(cli());
    daemon
        .kill_on_drop(true)
        .args(["daemon", "--socket"])
        .arg(&socket);
    let refused = tokio::time::timeout(Duration::from_secs(10), daemon.output())
        .await
        .expect("credential refusal must precede daemon startup")
        .unwrap();
    assert!(
        !refused.status.success(),
        "daemon started without its credentials"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("cannot open existing credential file")
    );
    assert!(!path.exists());
    assert!(!socket.exists());
}

#[tokio::test]
async fn trial_does_not_import_or_overwrite_legacy_credentials() {
    let root = tempfile::tempdir().unwrap();
    let credentials = root.path().join("credentials");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&credentials)
        .unwrap();
    let init = command(root.path(), "legacy")
        .args(["daemon", "init", "--username", "trial", "--passwordless"])
        .output()
        .unwrap();
    assert!(init.status.success());
    let db = root.path().join("legacy/eidetica.db");
    assert!(
        has_legacy_secret(&db).await,
        "legacy positive control must store the key in SQL"
    );
    let before = fs::read(&db).unwrap();
    let refused = command(root.path(), "legacy")
        .arg("info")
        .arg("--device-key-dir")
        .arg(&credentials)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("legacy instance credentials"));
    assert!(fs::read_dir(credentials).unwrap().next().is_none());
    assert!(
        fs::read(db).unwrap() == before,
        "legacy SQLite image changed"
    );
    let legacy = command(root.path(), "legacy").arg("info").output().unwrap();
    assert!(
        legacy.status.success(),
        "unchanged legacy mode cannot reopen its own database"
    );
}
