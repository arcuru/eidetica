//! The actual daemon entry point must apply the same policy as the library.
use eidetica::service::{RemoteConnection, ServiceConfig};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn binary() -> Command {
    let mut cmd = Command::new(
        std::env::var_os("NEXTEST_BIN_EXE_eidetica")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_eidetica").into()),
    );
    cmd.env_remove("EIDETICA_MAX_FRAME_BYTES");
    cmd
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn daemon_entry_point_applies_cli_and_env_frame_limits() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let init = binary()
        .args(["daemon", "--backend", "sqlite", "--data-dir"])
        .arg(&data)
        .args(["init", "--username", "admin", "--passwordless"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    for (index, (bytes, env)) in [(128, false), (128, true), (1073741824, false)]
        .into_iter()
        .enumerate()
    {
        let socket = dir.path().join(format!("daemon-{index}.sock"));
        let mut cmd = binary();
        cmd.args(["daemon", "--backend", "sqlite", "--data-dir"])
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if env {
            cmd.env("EIDETICA_MAX_FRAME_BYTES", bytes.to_string());
        } else {
            cmd.args(["--max-frame-bytes", &bytes.to_string()]);
        }
        let mut daemon = Daemon(cmd.spawn().unwrap());
        let conn = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    daemon.0.try_wait().unwrap().is_none(),
                    "daemon exited before binding"
                );
                if socket.exists() {
                    break RemoteConnection::connect(&socket).await.unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon did not accept a handshake");
        let response = tokio::time::timeout(Duration::from_secs(5), conn.get_instance_metadata())
            .await
            .expect("daemon frame limit stranded caller");
        if bytes == 128 {
            assert!(response.is_err(), "CLI limit was ignored");
        } else {
            assert!(response.unwrap().is_some());
        }
        drop(conn);
        drop(daemon);
    }
    assert_eq!(ServiceConfig::default().max_frame_bytes(), 67108864);
    for bytes in ["0", "4294967296", "18446744073709551616"] {
        let invalid = binary()
            .args(["daemon", "--max-frame-bytes", bytes])
            .output()
            .unwrap();
        assert_eq!(invalid.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("--max-frame-bytes"));
    }
    let invalid_env = binary()
        .args(["daemon"])
        .env("EIDETICA_MAX_FRAME_BYTES", "0")
        .output()
        .unwrap();
    assert_eq!(invalid_env.status.code(), Some(2));
}
