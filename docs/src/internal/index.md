# Eidetica Architecture Overview

Eidetica is a decentralized database designed to "Remember Everything." This document outlines the architecture and how different components interact with each other.

Eidetica is built on a foundation of content-addressable entries organized in databases, with a pluggable backend system for storage. `Entry` objects are immutable and include integrated authentication using Ed25519 digital signatures. The database is designed with concepts from Merkle-CRDTs to enable efficient merging and synchronization of distributed data.

See the [Core Components](core_components/index.md) section for details on the key building blocks.

```mermaid
graph TD
    A[User Application] --> B[Instance]
    B --> C[Database]
    C --> E[Database]
    E --> F[InMemoryDatabase]
    E -.-> G[Other Databases]

    subgraph Entry Creation and Structure
        H[EntryBuilder] -- builds --> D[Entry]
        D -- contains --> I[TreeNode]
        D -- contains --> J[SubTreeNode]
        D -- contains --> K[SigInfo]
        L[AuthValidator] -- validates --> D
        L -- uses --> M[_settings.auth]
    end

    subgraph Authentication Module
        K --> N[SigKey]
        K --> O[Signature]
        L --> P[ResolvedAuth]
        Q[CryptoModule] -- signs/verifies --> D
    end

    B -- creates --> C
    C -- manages --> D
    B -- uses --> H
    C -- uses --> E
    C -- uses --> L
```
