# Eidetica Architecture Overview

Eidetica is a decentralized database designed to "Remember Everything." This document outlines the architecture and how different components interact with each other.

Eidetica is built on a foundation of content-addressable entries organized in databases, with a pluggable backend system for storage. `Entry` objects are immutable and contain Tree/SubTree structures that form the Merkle-DAG, with integrated authentication using Ed25519 digital signatures. The system provides Database and Store abstractions over these internal structures to enable efficient merging and synchronization of distributed data.

See the [Core Components](core_components/index.md) section for details on the key building blocks.

```mermaid
graph TD
    A[User Application] --> B[Instance]
    B --> C[Database]
    C --> T[Transaction]
    T --> S[Stores: DocStore, Table, etc.]

    subgraph Backend Layer
        C --> BE[Backend: InMemory, etc.]
        BE --> D[Entry Storage]
    end

    subgraph Entry Internal Structure
        H[EntryBuilder] -- builds --> E[Entry]
        E -- contains --> I[TreeNode]
        E -- contains --> J[SubTreeNode Vector]
        E -- contains --> K[SigInfo]
        I --> IR[Root ID, Parents, Metadata]
        J --> JR[Name, Parents, Data]
    end

    subgraph Authentication System
        K --> N[SigKey]
        K --> O[Signature]
        L[AuthValidator] -- validates --> E
        L -- uses --> M[_settings subtree]
        Q[CryptoModule] -- signs/verifies --> E
    end

    subgraph User Abstractions
        C -.-> |"provides view over"| I
        S -.-> |"provides view over"| J
    end

    T -- uses --> H
    H -- stores --> BE
    C -- uses --> L
    S -- modifies --> J
```
