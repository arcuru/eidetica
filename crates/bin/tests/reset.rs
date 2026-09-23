use eidetica::{
    backend::{BackendImpl, VerificationStatus, database::InMemory},
    entry::Entry,
};
use std::{fs, process::Command};

fn reset(dir: &std::path::Path, confirm: bool) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetica"));
    cmd.args([
        "db",
        "reset-local-verification",
        "--backend",
        "inmemory",
        "--data-dir",
    ])
    .arg(dir);
    if confirm {
        cmd.arg("--confirm");
    }
    cmd.output().unwrap()
}

#[tokio::test]
async fn cli_reset_requires_acknowledgement_and_keeps_persisted_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eidetica.json");
    assert!(!reset(dir.path(), true).status.success());
    let backend = InMemory::new();
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    backend.put(entry.clone()).await.unwrap();
    backend
        .update_verification_status(&id, VerificationStatus::Verified)
        .await
        .unwrap();
    backend.save_to_file(&path).unwrap();
    assert!(!reset(dir.path(), false).status.success());
    assert!(reset(dir.path(), true).status.success());
    let loaded = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    assert_eq!(
        loaded.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(loaded.get(&id).await.unwrap(), entry);
    fs::write(&path, b"broken json").unwrap();
    assert!(!reset(dir.path(), true).status.success());
    assert_eq!(fs::read(&path).unwrap(), b"broken json");
}

#[tokio::test]
async fn cli_sqlite_reset_survives_reopen() {
    use eidetica::backend::database::Sqlite;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eidetica.db");
    let backend = Sqlite::open(&path).await.unwrap();
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    backend.put(entry.clone()).await.unwrap();
    backend
        .update_verification_status(&id, VerificationStatus::Failed)
        .await
        .unwrap();
    drop(backend);
    let result = Command::new(env!("CARGO_BIN_EXE_eidetica"))
        .args([
            "db",
            "reset-local-verification",
            "--backend",
            "sqlite",
            "--data-dir",
        ])
        .arg(dir.path())
        .arg("--confirm")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let reopened = Sqlite::open(&path).await.unwrap();
    assert_eq!(
        reopened.get_verification_status(&id).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(reopened.get(&id).await.unwrap(), entry);
}
