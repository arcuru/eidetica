//! Exercise host configuration through the packaged command boundary, not a mock.
#![cfg(target_os = "linux")]

use std::{fs, path::Path, process::Command};

fn command(home: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(
        std::env::var_os("NEXTEST_BIN_EXE_eidetica")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica").into()),
    );
    cmd.env_remove("EIDETICA_CACHE_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env("HOME", home)
        .env("EIDETICA_DATA_DIR", data);
    cmd
}

fn succeeds(cmd: &mut Command) {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cache_files(directory: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sqlite"))
        .collect()
}

#[test]
fn host_defaults_overrides_maintenance_and_offline_recreation_share_one_directory() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let xdg = tmp.path().join("xdg");
    let shared = xdg.join("eidetica");
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    for data in [&first, &second] {
        succeeds(command(&home, data).env("XDG_CACHE_HOME", &xdg).args([
            "daemon",
            "init",
            "--username",
            "admin",
            "--passwordless",
        ]));
        // No persistent cache name is invented before complete metadata exists.
        assert_eq!(cache_files(&shared).len(), usize::from(data == &second));
        succeeds(command(&home, data).env("XDG_CACHE_HOME", &xdg).arg("info"));
        assert!(!data.join("eidetica.db.cache").exists());
    }
    let files = cache_files(&shared);
    assert_eq!(files.len(), 2);
    assert_ne!(files[0], files[1]);

    // HOME fallback, then explicit directory with existing group permissions.
    let third = tmp.path().join("third");
    succeeds(command(&home, &third).args([
        "daemon",
        "init",
        "--username",
        "admin",
        "--passwordless",
    ]));
    succeeds(command(&home, &third).arg("info"));
    assert_eq!(cache_files(&home.join(".cache/eidetica")).len(), 1);
    let custom = tmp.path().join("custom");
    fs::create_dir(&custom).unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o770)).unwrap();
    succeeds(
        command(&home, &first)
            .args(["info", "--cache-dir"])
            .arg(&custom),
    );
    let cache = cache_files(&custom).pop().unwrap();
    fs::remove_file(&cache).unwrap(); // No live owner; keep stable lock sidecar.
    let before = fs::read(first.join("eidetica.db")).unwrap();
    succeeds(
        command(&home, &first)
            .env("EIDETICA_CACHE_DIR", &custom)
            .arg("info"),
    );
    assert!(cache.is_file());
    assert_eq!(fs::read(first.join("eidetica.db")).unwrap(), before);
    succeeds(
        command(&home, &first)
            .env("EIDETICA_CACHE_DIR", &custom)
            .args(["db", "list"]),
    );
    succeeds(
        command(&home, &first)
            .env("EIDETICA_CACHE_DIR", &custom)
            .args(["db", "reset-local-verification", "--confirm"]),
    );
    assert!(
        !first.join("eidetica.db.cache").exists(),
        "maintenance must honor host directory configuration"
    );
    assert_eq!(cache_files(&custom), [cache]);
    assert_eq!(
        fs::metadata(&custom).unwrap().permissions().mode() & 0o7777,
        0o770
    );

    let not_a_dir = tmp.path().join("not-a-directory");
    fs::write(&not_a_dir, b"preserve").unwrap();
    assert!(
        !command(&home, &first)
            .args(["info", "--cache-dir"])
            .arg(&not_a_dir)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(not_a_dir).unwrap(), b"preserve");
}
