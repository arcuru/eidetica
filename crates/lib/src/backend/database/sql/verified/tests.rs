//! Real PostgreSQL interleavings at the status/prefix transaction boundary.
//!
//! SQL triggers park admission after eligibility reads, without changing the
//! production Rust path. Each test uses an isolated schema in the test fixture.

use std::{sync::Arc, time::Duration};

use sqlx::{AssertSqlSafe, Connection, PgConnection};
use tokio::task::JoinHandle;

use super::*;
use crate::{backend::BackendImpl, entry::Entry};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum Competitor {
    DemoteAncestor,
    DemoteChild,
    Rebuild,
}

/// Wait for the real INSERT to reach the advisory-lock barrier. Polling
/// pg_locks establishes the interleaving; the delay only limits polling load.
async fn admission_waiter(connection: &mut PgConnection, key: i64) -> i64 {
    tokio::time::timeout(WAIT, async {
        loop {
            let waiter: Option<(i64,)> = sqlx::query_as(
                "SELECT pid::bigint FROM pg_locks WHERE locktype = 'advisory' \
                 AND classid = 0 AND objid::bigint = $1 AND NOT granted",
            )
            .bind(key)
            .fetch_optional(&mut *connection)
            .await
            .unwrap();
            if let Some((pid,)) = waiter {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("promotion must reach the post-eligibility INSERT barrier")
}

/// The broken implementation may finish the competing transaction, or block
/// on the entry row after reading a stale status. The fixed implementation
/// blocks before mutable reads. In either case, observe a database event
/// before releasing admission, rather than depending on scheduler timing.
async fn competitor_blocked_or_finished(
    connection: &mut PgConnection,
    admission_pid: i64,
    task: &JoinHandle<Result<()>>,
) -> bool {
    tokio::time::timeout(WAIT, async {
        loop {
            if task.is_finished() {
                return false;
            }
            let (blocked,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE CAST($1 AS INTEGER) = ANY(pg_blocking_pids(pid)))",
            )
            .bind(admission_pid)
            .fetch_one(&mut *connection)
            .await
            .unwrap();
            if blocked {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("competing operation must finish or block on admission")
}

async fn admission_race(competitor: Competitor) {
    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
    let backend = Arc::new(SqlxBackend::connect_postgres_isolated(&url).await.unwrap());
    let root = Entry::root_builder().build().unwrap();
    let root_id = root.id();
    let child = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(1)
        .set_subtree_data("test", b"child")
        .build()
        .unwrap();
    let child_id = child.id();
    backend.put(root).await.unwrap();
    backend.put(child).await.unwrap();
    backend
        .update_verification_status(&root_id, VerificationStatus::Verified)
        .await
        .unwrap();

    // Hold a transaction-scoped barrier so failure/drop also releases it.
    // The random positive 31-bit key isolates parallel tests' advisory locks.
    let key = (uuid::Uuid::new_v4().as_u128() % i32::MAX as u128) as i64;
    // Keep the observer outside the backend's two-connection test pool so
    // both competing BackendImpl transactions can run at the same time.
    let mut observer = PgConnection::connect(&url).await.unwrap();
    let mut barrier = observer.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *barrier)
        .await
        .unwrap();
    // SAFETY: the function interpolates only an integer, and the trigger only
    // a canonical CID (base32, with no SQL metacharacters), never input SQL.
    sqlx::query(AssertSqlSafe(format!(
        "CREATE FUNCTION pause_admission() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN PERFORM pg_advisory_xact_lock({key}); RETURN NEW; END $$"
    )))
    .execute(backend.pool())
    .await
    .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "CREATE TRIGGER pause_admission BEFORE INSERT ON verified_prefix \
         FOR EACH ROW WHEN (NEW.entry_id = '{child_id}') \
         EXECUTE FUNCTION pause_admission()"
    )))
    .execute(backend.pool())
    .await
    .unwrap();

    let promote = {
        let backend = backend.clone();
        let child_id = child_id.clone();
        tokio::spawn(async move {
            backend
                .update_verification_status(&child_id, VerificationStatus::Verified)
                .await
        })
    };
    let pid = admission_waiter(&mut barrier, key).await;
    let competing = {
        let backend = backend.clone();
        let root_id = root_id.clone();
        let child_id = child_id.clone();
        tokio::spawn(async move {
            match competitor {
                Competitor::DemoteAncestor => {
                    backend
                        .update_verification_status(&root_id, VerificationStatus::Failed)
                        .await
                }
                Competitor::DemoteChild => {
                    backend
                        .update_verification_status(&child_id, VerificationStatus::Failed)
                        .await
                }
                Competitor::Rebuild => backend.rebuild_verified_state(&root_id).await.map(|_| ()),
            }
        })
    };
    let blocked = competitor_blocked_or_finished(&mut barrier, pid, &competing).await;
    barrier.rollback().await.unwrap();
    for task in [promote, competing] {
        tokio::time::timeout(WAIT, task)
            .await
            .expect("operation must finish after admission is released")
            .expect("operation must not panic")
            .unwrap();
    }
    sqlx::query("DROP TRIGGER pause_admission ON verified_prefix")
        .execute(backend.pool())
        .await
        .unwrap();

    // Inspect retained state before the repair oracle can mask a stale prefix.
    let retained = backend.verified_snapshot(&root_id).await.unwrap();
    let rebuilt = backend.rebuild_verified_state(&root_id).await.unwrap();
    assert_eq!(
        retained, rebuilt,
        "concurrent status/prefix decisions must equal cold reconstruction"
    );
    let expected = match competitor {
        Competitor::DemoteAncestor => {
            assert_eq!(
                backend.get_verification_status(&root_id).await.unwrap(),
                VerificationStatus::Failed
            );
            Snapshot::default()
        }
        Competitor::DemoteChild => {
            assert_eq!(
                backend.get_verification_status(&child_id).await.unwrap(),
                VerificationStatus::Failed
            );
            Snapshot::from([root_id])
        }
        Competitor::Rebuild => {
            assert!(blocked, "explicit rebuild must serialize with admission");
            Snapshot::from([child_id])
        }
    };
    assert_eq!(retained, expected);
}

#[tokio::test]
async fn postgres_admission_racing_ancestor_demotion() {
    admission_race(Competitor::DemoteAncestor).await;
}

#[tokio::test]
async fn postgres_admission_racing_same_entry_demotion() {
    admission_race(Competitor::DemoteChild).await;
}

#[tokio::test]
async fn postgres_admission_serializes_explicit_rebuild() {
    admission_race(Competitor::Rebuild).await;
}
