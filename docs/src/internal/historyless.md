# Historyless Storage Architecture

A historyless database is an authoritative current-state catalog alongside the entry DAG.
Its stable `ID` is opaque and its database-wide revision is the compare-and-swap boundary for every Store.

## Backend records

The authoritative tables are `historyless_databases`, `historyless_store_heads`, `store_state_namespaces`, and `store_state_records`.
Record keys and values are opaque bytes; `(namespace_id, record_key)` supplies point lookup and ordered scans.
SQL commits create immutable revision namespaces and swap heads in the same transaction as the revision CAS.
Older revision namespaces remain readable only while a captured revision can select them; physical cleanup is backend lifecycle work.

InMemory mirrors the same model with immutable revision values and explicit read pins.
A commit replaces the catalog's current revision without mutating pinned values.
Releasing the last pin permits the superseded value to be reclaimed; it is never exposed as user-visible history.

SQL schema version 2 creates the shared Store-state tables and heads.
The v1-to-v2 migration converts each unreleased `historyless_stores.state` blob into one legacy authoritative record without interpreting Store type.
The legacy table remains migration input and is not consulted by normal reads or commits.

## Lifecycle isolation

Derived historical projections and historyless authority use the same record substrate but different lifecycle metadata.
Derived clearing selects only `Derived`; it cannot select authoritative namespaces or heads.
Historyless IDs remain absent from entry enumeration.
