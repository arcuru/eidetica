# DB Design

unimplemented!();

The database needs to be distributed, and able to sync only the most recent data without issue.

The design constraints encourage it to be decentralized as well, for maximum flexibility.

It consists of a couple layers, but central is the Metadata table that tracks the full history.

It is intended to be reasonably efficient for active use, but primarily as an archival and change-tracking tool.

## Metadata Table Schema

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

The actual data is assumed to have a unique hash for actual reference.

Any additional info needed will be stored in the Metadata field, and entries in that field that prove useful may be added to the Schema in future.

Sync can be done by connecting to a device, and requesting "Here's the latest I have from you, give me newer stuff".
It should also be safe to sync in either direction.

Active/Non-deleted data can be referenced by checking the Archive bit.
A delete is a new entry with no data, that marks the parents archive bit.

The database is a Directed Acyclic Graph, with pointers going back in time so that you can view the full history of any item.
This design makes it an expensive operation to find the children of a node, but cheap to find all the parents.

## Data Storage

Fundamentally, the Metadata info is shared across all devices, but the actual data is not.
So you can separately sync the Metadata/history without syncing the data.
You can also delete/move/archive data without modifying the metadata.

The data is stored by its hash in a separate table.
It can be either stored inline in that table, in a local file, in S3, or as a reference to another Eidetica node.
At some point, it may be possible to store it as a diff on top of another piece of data.
The data may not necessarily be present, so we may only have links/hints as to where the data is.

Local devices will use caching strategies to keep recent or frequently accessed data.

There will be a setting of "store up to XX GB locally", possible to set globally for Eidetica and per "Data Store".
