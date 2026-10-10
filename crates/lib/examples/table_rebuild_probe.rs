//! Separate persisted-fixture construction from a one-shot cold read process.
//! Usage: table_rebuild_probe prepare|read PATH ROWS plain|encrypted
use eidetica::{
    Database, ID, Instance, NewUser,
    backend::database::Sqlite,
    crdt::Doc,
    store::{PasswordStore, Table},
};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::Path};
use tokio::runtime::Runtime;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    id: usize,
    name: String,
    value: i64,
}

fn rss() -> (u64, u64) {
    let s = fs::read_to_string("/proc/self/status").unwrap();
    let field = |key: &str| {
        s.lines()
            .find(|line| line.starts_with(key))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    (field("VmRSS:"), field("VmHWM:"))
}

async fn prepare(path: &Path, rows: usize, encrypted: bool) {
    assert!(rows > 0 && !path.exists());
    let (instance, mut user) = Instance::create_backend(
        Box::new(Sqlite::open(path).await.unwrap()),
        NewUser::passwordless("bench_user"),
    )
    .await
    .unwrap();
    let key = user.get_default_key().unwrap();
    let db = user.create_database(Doc::new(), &key).await.unwrap();
    fs::write(path.with_extension("root"), db.root_id().to_string()).unwrap();
    // Keep the same logical rows and 128-row Entry history on both revisions.
    for chunk in (0..rows).step_by(128) {
        let tx = db.new_transaction().await.unwrap();
        if encrypted {
            let mut wrapped = tx
                .get_store::<PasswordStore<Table<Row>>>("bench_table")
                .await
                .unwrap();
            if chunk == 0 {
                wrapped
                    .initialize("bench-password", Doc::new())
                    .await
                    .unwrap();
            } else {
                wrapped.open("bench-password").unwrap();
            }
            let table = wrapped.inner().await.unwrap();
            for id in chunk..(chunk + 128).min(rows) {
                table
                    .set(
                        format!("row-{id:08}"),
                        Row {
                            id,
                            name: format!("record_{id}"),
                            value: id as i64 * 100,
                        },
                    )
                    .await
                    .unwrap();
            }
        } else {
            let table = tx.get_store::<Table<Row>>("bench_table").await.unwrap();
            for id in chunk..(chunk + 128).min(rows) {
                table
                    .set(
                        format!("row-{id:08}"),
                        Row {
                            id,
                            name: format!("record_{id}"),
                            value: id as i64 * 100,
                        },
                    )
                    .await
                    .unwrap();
            }
        }
        tx.commit().await.unwrap();
    }
    // Unlink then reclaim a reader-pinned previous derived generation.
    for _ in 0..2 {
        db.backend()
            .unwrap()
            .clear_derived_store_state()
            .await
            .unwrap();
    }
    drop(db);
    drop(user);
    drop(instance);
    println!("prepared rows={rows} encrypted={encrypted}");
}

async fn read(path: &Path, rows: usize, encrypted: bool) {
    let instance = Instance::open_backend(Box::new(Sqlite::open(path).await.unwrap()))
        .await
        .unwrap();
    let root = ID::parse(
        fs::read_to_string(path.with_extension("root"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    let db = Database::open(&instance, &root).await.unwrap();
    let before = rss();
    let start = std::time::Instant::now();
    let key = format!("row-{:08}", rows / 2);
    let row = if encrypted {
        let tx = db.new_transaction().await.unwrap();
        let mut wrapped = tx
            .get_store::<PasswordStore<Table<Row>>>("bench_table")
            .await
            .unwrap();
        wrapped.open("bench-password").unwrap();
        wrapped.inner().await.unwrap().get(&key).await.unwrap()
    } else {
        db.get_store_viewer::<Table<Row>>("bench_table")
            .await
            .unwrap()
            .get(&key)
            .await
            .unwrap()
    };
    assert_eq!(row.id, rows / 2);
    let after = rss();
    println!(
        "read rows={rows} encrypted={encrypted} before_rss_kib={} before_hwm_kib={} after_rss_kib={} after_hwm_kib={} elapsed_ms={:.3} row_id={}",
        before.0,
        before.1,
        after.0,
        after.1,
        start.elapsed().as_secs_f64() * 1000.0,
        row.id
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    assert_eq!(args.len(), 5, "mode path rows plain|encrypted");
    let rows: usize = args[3].parse().unwrap();
    assert!(args[4] == "plain" || args[4] == "encrypted");
    let rt = Runtime::new().unwrap();
    let path = Path::new(&args[2]);
    match args[1].as_str() {
        "prepare" => rt.block_on(prepare(path, rows, args[4] == "encrypted")),
        "read" => rt.block_on(read(path, rows, args[4] == "encrypted")),
        _ => panic!("mode must be prepare or read"),
    }
}
