# Eidetica Docs

Docs describe either the current state or the planned design.

Unimplemented sections include `unimplemented!();`, which is the Rust macro for noting unimplemented code.

The intent of the design is to be a generic, distributed, archival system that tracks changes and interfaces with advanced searching.

The impetus was from using the current crop of AI's, where I want a system to retrieve full context from my personal storage.
This necessitates either waiting for all the disparate storage providers I use to implement context retrieval...or move everything into a generic system that does.

## Data Organization

unimplemented!();

Data stored in Eidetica is stored in "Data Stores", name TBD, that are groups of related data managed together.

A "Data Store" may contain different types of data, but should be logically grouped.
Names are from the user.

As an example, an "Email" data store would contain all Emails and attachments.

The intention is that you will be able to set "sync all my email locally" or "sync the last year of emails locally" or "sync the most recent X GB of email locally" or "index all emails for searching".

Each "Data Store" will have it's own table/set of tables for sharding and efficient syncing.

## Plugins

unimplemented!();

While the core of this tool will be the sync database and searching, plugins will be necessary for handling and interpreting the data.

e.g. Email. The email text (or email + it's own metadata) will be stored, but Eidetica core won't know how to interpret it.
An email plugin will know how to format it for search, and potentially handle converting some input format as well.
It will also handle display.

My list of key plugins that I have kept in mind during the design phase.

- Email
- Chat
- Atuin (cmd line history)
- Files (full file sync)
- Web Browsing history
- Custom Application data

Perhaps not in the first version, but the intention is to allow people to run separate plugins outside of Eidetica with an API.
So you could write your own plugin and use it externally to the Eidetica project.

TBD on what the interface needs to look like.

## Database Design

unimplemented!();

The database needs to be distributed, and able to sync only the most recent data without issue.

The design constraints encourage it to be decentralized as well, for maximum flexibility.

It consists of a couple layers, but central is the Metadata table that tracks the full history.

It is intended as reasonably efficient for active use, but primarily as an archival and change-tracking tool.

### Metadata Table Schema

The Metadata table is logically append-only, but not necessarily in practice.
Hopefully that does not cause problems.

This is the table form of how devices will communicate.

Each entry in the table contains the following data

| Name         | Type   | Description                                                                                                           |
| ------------ | ------ | --------------------------------------------------------------------------------------------------------------------- |
| ID           | UUIDv7 | Increasing id that is guaranteed\* unique from any device                                                             |
| Device ID    | UUID?? | Unique Device ID where this data was inserted                                                                         |
| Archived bit | bool   | Indicating if this data has been replaced by a newer row. This bit can be modified only in the direction of archival. |
| Parent       | UUIDv7 | Optional, Reference to the UUID of the parent of this change                                                          |
| Metadata     | json   | Metadata about the referenced data                                                                                    |
| Data         | blob   | The data actually being stored, or a reference to it.                                                                 |

The ID is generated at the time of insertion, and _should_ be unique across any device.
The likelihood of a collision is ridiculously small.

A timestamp for the time of insertion is implicit in the ID, as it contains a timestamp + a random set of digits.

The actual data is assumed to have a unique SHA hash for actual reference.

Any additional info needed will be stored in the Metadata field, and entries in that field that prove useful may be added to the Schema in future.

Sync can be done by connecting to a device, and requesting "Here's the latest I have from you, give me newer stuff".
It should also be safe to sync in either direction.

Active/Non-deleted data can be referenced by checking the Archive bit.
A delete is a new entry with no data, that marks the parents archive bit.

The database is a Directed Acyclic Graph, with pointers going back in time so that you can view the full history of any item.
This design makes it an expensive operation to find the children of a node, but cheap to find all the parents.

### Data Storage

Fundamentally, the Metadata info is shared across all devices, but the actual data is not.
So you can separately sync the Metadata/history without syncing the data.
You can also delete/move/archive data without modifying the metadata.

The data is stored by its hash in a separate table.
It can be either stored inline in that table, in a local file, in S3, or as a reference to another Eidetica node.
At some point, it may be possible to store it as a diff on top of another piece of data.
The data may not necessarily be present, so we may only have links/hints as to where the data is.

Local devices will use caching strategies to keep recent or frequently accessed data.

There will be a setting of "store up to XX GB locally", possible to set globally for Eidetica and per "Data Store".

### Search

Search will be done by indexing data into ElasticSearch.
The indexed data will be configurable, and should be as small as possible for performance reasons.

Plugins will be responsible for deciding how the data blobs are formatted into ES.

### Encryption

unimplemented!();

End-to-End encryption should be relatively easy to implement from a technical level.
Similar to how Syncthing does it, we'll also be able to support having some nodes unable to see the data at all.

We can just create a plugin for Encryption that wraps any other module underneath it.
The data and optionally metadata can be encrypted using a key only the user has that Eidetica will never sync.

The user can enter that key into an instance of Eidetica where they want to be able to read the data, and the Encryption plugin will handle unwrapping.
