# Eidetica

[![build](https://img.shields.io/github/actions/workflow/status/arcuru/eidetica/rust.yml?style=flat-square)](https://github.com/arcuru/eidetica/actions)
[![coverage](https://img.shields.io/codecov/c/github/arcuru/eidetica)](https://codecov.io/gh/arcuru/eidetica)
![license](https://img.shields.io/github/license/arcuru/eidetica)

Remember Everything. Everywhere. All at once.

I wanted to search my email, so I started by building a decentralized database.

It made sense at the time...

Eidetica will allow you to write a [local-first](https://www.inkandswitch.com/essay/local-first), peer-to-peer, decentralized application just by storing your data here.
An app developer does not need to deal with anything else, just use the database interface provided here to store data, add a user flow to initialize sync and "login" to the network, and everything else will be handled for you.

It's not _quite_ all there yet, but I'm working on it. Peer-to-Peer connectivity is only minimal at the moment.

Extensive, but largely LLM Generated/Human Curated documentation is maintained under docs/ and [hosted on github.io.](https://arcuru.github.io/eidetica)

Blog posts that I've written about Eidetica: [https://jackson.dev/tags/eidetica/](https://jackson.dev/tags/eidetica/)

Discuss on Matrix:

- [Space (if your client supports it)](https://matrix.to/#/#eidetica:jackson.dev)
- [General Room](https://matrix.to/#/#eidetica-general:jackson.dev)

### EXPERIMENTAL

Everything is still experimental, and very much in-development.
That includes the underlying storage format, which is still GUARANTEED to change between now and declaring 1.0.

No backwards compatibility OR MIGRATION PATH is guaranteed before 1.0.

## Vision

- Peer-to-Peer Database, built on top of layered [Merkle-CRDTs](https://arxiv.org/abs/2004.00107)
- Direct connections using [iroh](https://www.iroh.computer/)
- Multi-transport sync (HTTP + Iroh P2P simultaneously)
- Decentralized Authentication
- Built in Rust for native compilation and broad platform support
- Efficient Sync with a data model aware API (TODO)
- P2P Object Storage across the same sync network (TODO)
- Sparse and Shallow views with 0 extra overhead (TODO, data model already supports)
- WASM for embedding it into webapps (TODO)

This is an attempt to combine a few successful decentralized approaches into something more unified, suitable for a storage layer of the decentralized web.
There are a few synergies that I am taking advantage of to make each component work together efficiently, so it is more valuable to co-design the Sync API, Storage format, Authentication scheme, etc.

I wanted this for a different project and wasn't able to find something with everything I wanted, so here we are.

Eidetica lets you make a decentralized app, so long as you store your DB (and eventually, Objects), in the storage format here. It manages storage, authentication, sync, conflict resolution, and encryption. Future work includes better peer sync and discovery, user management, sparse checkouts, and many more features.

Contained here are both a library suitable for embedding in your own applications and a binary that will be a hostable multi-user synchronization node.

Under the hood it uses the Merkle-CRDT concept pioneered by [OrbitDB](https://orbitdb.org/) to create an eventually consistent database by providing a total ordering of database operations to a CRDT, and extends it by layering multiple DAG's into the underlying structure.
This does have downsides, and it is not suitable for all applications, but I think I can take the concept quite far by extending that architecture out into the rest of the system.

OrbitDB relies on IPFS and libp2p to sync, but by writing custom synchronization protocols that understand the data structures I expect that there are large efficiency gains to be had.

The library is extremely modular. It is simple to define your own CRDT, your own custom Backend, even your own transport layer if you need to support Bluetooth/Zigbee/etc communication.

See the docs/ for a user guide, examples, internal architecture, and full design docs explaining design decisions.

## Data Model

The user facing data model is intentionally designed to match existing Databases closely.

- **Database** - The fundamental unit of authentication, synchronization, and storage.
- **Store** - Part of a Database, think "Table" but more general.
- **Transaction** - An Atomic Operation across any number of Stores in a Database.

Stores can be any CRDT, even user defined.
At the moment Eidetica provides a `Doc` type that is a generic nested Document type, a `Table` that functions as a NoSQL-ish Table for any data you want, and a `PasswordStore` that wraps any store with transparent password-based encryption.
Behind a feature flag is also a wrapper for the popular Y-Doc CRDT type.
This can be extended with more custom types to match user patterns.

Provided data types all use Last-Write-Wins conflict resolution, but facilities for other strategies will be provided.

A Transaction can operate across multiple Stores, so you can synchronize your changes to multiple Tables/Docs/etc.

Eventually, you will be able to sparsely checkout just particular Stores within a Database while still being able to locally verify the full history, and shallow checkout the current state from the network.

## Contributing

At this time, outside contributions will not be accepted due to [licensing considerations](https://jackson.dev/post/oss-licensing-sucks).

## Repository

Mirrored on [GitHub](https://github.com/arcuru/eidetica) and [Codeberg](https://codeberg.org/arcuru/eidetica).

For practical reasons GitHub is the official repo, but use either repo to contribute.
Issues can't be synced so there may be some duplicates.
