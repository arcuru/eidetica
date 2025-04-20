## Future Development

Based on comments in the code and potential architectural needs, future development areas include:

<!-- TODO: Link to relevant GitHub issues or design documents for these items if they exist. -->

- **CRDT Refinement**: Further refinement of the CRDT data type implementation and trait usage (`RawData` vs. structured CRDT types). Ensuring robust merge logic and exploring more sophisticated CRDT types.
- **Security**: Implementing security features like entry signing and key management (TODOs noted in `entry.rs`).
- **Persistent Backends**: Developing persistent storage backends beyond the current `InMemoryBackend` (e.g., RocksDB, Sled, file-system based).
- **Blob Storage Integration**: Exploring integration with systems like IPFS for storing large binary blobs referenced by entries (commented out code exists in `basedb.rs`).
- **Querying & Filtering**: Enhancing tree operations for more complex querying and filtering capabilities beyond simple gets or subtree iterations.
- **Additional CRDTs**: Implementing additional CRDT implementations beyond the basic `KVOverWrite` (e.g., Sequences, Sets, Counters).
- **Replication & Networking**: Designing and implementing protocols for peer-to-peer replication and synchronization between nodes.
- **Indexing**: Adding indexing mechanisms to speed up lookups and queries, especially for large datasets.
- **Concurrency Improvements**: Investigating and potentially implementing alternative concurrency control mechanisms to improve performance under high load (see [Performance Considerations](../performance.md)).
