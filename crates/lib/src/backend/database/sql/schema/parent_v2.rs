// Verification-only v2 DDL fixture, before bounded-cache columns.
// Copied from the verification-only schema, not generated from the schema under test.
const PARENT_V2: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS schema_version (
        version BIGINT PRIMARY KEY
    )"#,
    r#"CREATE TABLE IF NOT EXISTS entries (
        id TEXT PRIMARY KEY NOT NULL,
        tree_id TEXT NOT NULL,
        is_root BIGINT NOT NULL DEFAULT 0,
        verification_status BIGINT NOT NULL DEFAULT 0,
        height BIGINT NOT NULL DEFAULT 0,
        entry_cbor BYTEA NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tree_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS subtrees (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        height BIGINT NOT NULL,
        data BLOB,
        PRIMARY KEY (entry_id, store_name)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS store_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id, store_name)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tips (
        entry_id TEXT NOT NULL,
        tree_id TEXT NOT NULL,
        store_name TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (entry_id, tree_id, store_name)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS verified_tips (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        PRIMARY KEY (tree_id, entry_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS instance_metadata (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS instance_secrets (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS store_state_namespaces (
        namespace_id TEXT PRIMARY KEY NOT NULL,
        database_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        lifecycle BIGINT NOT NULL,
        status BIGINT NOT NULL,
        scope_user_uuid TEXT NOT NULL,
        projection_name TEXT NOT NULL,
        projection_version BIGINT NOT NULL,
        source_key BYTEA NOT NULL,
        created_revision BIGINT,
        UNIQUE (database_id, store_name, lifecycle, status, scope_user_uuid,
                projection_name, projection_version, source_key)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS store_state_records (
        namespace_id TEXT NOT NULL,
        record_key BYTEA NOT NULL,
        record_value BYTEA,
        PRIMARY KEY (namespace_id, record_key),
        -- Dropping a namespace drops its records. SQLite enforces this because
        -- sqlx sets `PRAGMA foreign_keys = ON` on every connection it opens;
        -- it is off in a bare sqlite3 session, which makes this look inert.
        FOREIGN KEY (namespace_id) REFERENCES store_state_namespaces(namespace_id)
            ON DELETE CASCADE
    )"#,
    r#"CREATE INDEX IF NOT EXISTS idx_entries_tree_id ON entries(tree_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_entries_tree_height ON entries(tree_id, height DESC, id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_entries_verification ON entries(verification_status)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_entries_is_root ON entries(is_root)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_tree_parents_parent ON tree_parents(parent_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_tree_parents_child ON tree_parents(child_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_subtrees_tree_store_height ON subtrees(tree_id, store_name, height DESC, entry_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_subtrees_store_height ON subtrees(store_name, height DESC, entry_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_store_parents_parent ON store_parents(store_name, parent_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_store_parents_child ON store_parents(store_name, child_id)"#,
    r#"CREATE INDEX IF NOT EXISTS idx_tips_tree_store ON tips(tree_id, store_name)"#,
    "INSERT INTO schema_version (version) VALUES (2)",
];
