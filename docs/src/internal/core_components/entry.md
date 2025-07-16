# Entry

Fundamental immutable data unit in Eidetica's Merkle-DAG structure.

## Core Structure

**Tree Node**: Contains root ID, parent references, data, and optional metadata

**Subtrees**: Vector of named subtrees (like tables), each with independent parents and data

**Authentication**: Signature information including key ID and cryptographic signature

**Content-Addressable ID**: Unique hex-encoded SHA-256 hash of entry content ensuring integrity

## ID Generation

**Deterministic**: Based on canonical JSON serialization of entry data

**Canonical Form**: Parents and subtrees sorted alphabetically before hashing

**SHA-256 Hash**: Content hash formatted as hexadecimal string

**Thread Safe**: Simple string type for efficient sharing

## Entry Construction

**EntryBuilder**: Constructs entries with proper parent linkage and authentication

**Immutable**: Once created, entries cannot be modified

**Parent References**: Links to current tips form DAG structure

## Data Storage

**RawData**: Serialized string data (typically JSON)

**CRDT Integration**: Higher-level CRDT types serialize to/from RawData

**Metadata**: Optional non-merged data for operational efficiency

## Authentication Integration

**Mandatory Signing**: All entries require authentication information

**SigInfo**: Contains key reference and signature

**Canonical Signing**: Signature computed over entry without signature field

**Validation**: Every entry validated against authentication configuration

## Merkle-DAG Properties

**Parent Links**: References to previous entries form directed acyclic graph

**Content Integrity**: Hash-based IDs ensure tamper detection

**Immutability**: Entries never change after creation

**Efficient Verification**: Sparse checkout possible with metadata references
