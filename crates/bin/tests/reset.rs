use eidetica::{
    backend::{BackendImpl, VerificationStatus, database::InMemory},
    entry::Entry,
};
use std::{fs, process::Command};

fn eidetica() -> Command {
    // Nextest archives relocate binaries; keep Cargo's path for ordinary cargo test.
    Command::new(
        std::env::var_os("NEXTEST_BIN_EXE_eidetica")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica").into()),
    )
}

fn reset(dir: &std::path::Path, confirm: bool) -> std::process::Output {
    let mut cmd = eidetica();
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
    let result = eidetica()
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

fn repair(
    dir: &std::path::Path,
    backend: &str,
    tree: &eidetica::entry::ID,
    confirm: bool,
) -> std::process::Output {
    let mut cmd = eidetica();
    cmd.args([
        "db",
        "repair-verification",
        "--backend",
        backend,
        "--data-dir",
    ])
    .arg(dir)
    .arg("--tree")
    .arg(tree.to_string());
    if confirm {
        cmd.arg("--confirm");
    }
    cmd.output().unwrap()
}

#[tokio::test]
async fn cli_repair_is_explicit_conservative_and_refuses_unsupported_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eidetica.json");
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    assert!(!repair(dir.path(), "inmemory", &tree, true).status.success());
    assert!(!path.exists());
    let child = Entry::builder(tree.clone())
        .add_parent(tree.clone())
        .build()
        .unwrap();
    let child_id = child.id();
    let backend = InMemory::new();
    backend.put(root.clone()).await.unwrap();
    backend.put(child.clone()).await.unwrap();
    backend
        .update_verification_status(&tree, VerificationStatus::Failed)
        .await
        .unwrap();
    let mut value = serde_json::to_value(&backend).unwrap();
    value["verification_status"][child_id.to_string()] = serde_json::json!("Verified");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(
        !repair(dir.path(), "inmemory", &tree, false)
            .status
            .success()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let result = repair(dir.path(), "inmemory", &tree, true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("1 demoted to Unverified; 0 verified tips")
    );
    let loaded = InMemory::try_load_from_file(&path).await.unwrap().unwrap();
    assert_eq!(loaded.get(&tree).await.unwrap(), root);
    assert_eq!(loaded.get(&child_id).await.unwrap(), child);
    assert_eq!(
        loaded.get_verification_status(&tree).await.unwrap(),
        VerificationStatus::Failed
    );
    assert_eq!(
        loaded.get_verification_status(&child_id).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert!(
        String::from_utf8_lossy(&repair(dir.path(), "inmemory", &tree, true).stdout)
            .contains("0 demoted")
    );
    value["_v"] = serde_json::json!(1);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(!repair(dir.path(), "inmemory", &tree, true).status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::write(&path, b"broken json").unwrap();
    assert!(!repair(dir.path(), "inmemory", &tree, true).status.success());
    assert_eq!(fs::read(&path).unwrap(), b"broken json");
}

#[tokio::test]
async fn cli_sqlite_repair_preserves_valid_history_and_refuses_missing_or_corrupt_targets() {
    use eidetica::backend::database::Sqlite;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eidetica.db");
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    assert!(!repair(dir.path(), "sqlite", &tree, true).status.success());
    assert!(!path.exists());
    let backend = Sqlite::open(&path).await.unwrap();
    backend.put(root.clone()).await.unwrap();
    backend
        .update_verification_status(&tree, VerificationStatus::Verified)
        .await
        .unwrap();
    drop(backend);
    let result = repair(dir.path(), "sqlite", &tree, true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("0 demoted to Unverified; 1 verified tips")
    );
    let backend = Sqlite::open(&path).await.unwrap();
    assert_eq!(backend.get(&tree).await.unwrap(), root);
    assert_eq!(
        backend.get_verification_status(&tree).await.unwrap(),
        VerificationStatus::Verified
    );
    drop(backend);
    // A separate corrupt file has no valid WAL from an earlier open to recover.
    let corrupt = tempfile::tempdir().unwrap();
    let corrupt_path = corrupt.path().join("eidetica.db");
    fs::write(&corrupt_path, b"not a SQLite database").unwrap();
    assert!(
        !repair(corrupt.path(), "sqlite", &tree, true)
            .status
            .success()
    );
    assert_eq!(fs::read(&corrupt_path).unwrap(), b"not a SQLite database");
}
