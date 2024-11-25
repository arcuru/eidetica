# Data Organization

unimplemented!();

There are assumed to be several levels of organization in an Eidetica node.

## Global

An Eidetica node consists of 1 or more Users.

Global data is limited, but will include settings for the node in general and a global Data Table.

The Node is owned by a Key Pair, which Owns all User accounts on the instance.

The Global Key also defines a Data Stream, which is used for config updates to the instance.
For example, entries of "adding new user XXX" will be included in its data stream.

## Users

A user on an Eidetica node will have their own set of settings and preferences.
A user can own other users, Stores, and Streams.
All data goes into one of the owned Data Streams.

A User Key Pair is unique to an instance.

Users can own other users, which will allow them to functionally have the same rights as the other user.
For example, and Organization can own all it's Users.
Or if you have an account in 2 places, the accounts will mutually own each other so that each can act as the other.

This will allow bootstrapping a new device for example.
From the UI, you can create a new user, connect it to an existing one, and then we'll be able to query and find existing Data Stores and sync them.

Configuration updates for Users will also go into the User's Data Stream.

## Data Stores

A Data Store is a collection of Data Streams, or other Data Stores.
Mutual ownership is fine and encouraged.
You might prefer to call them "Folders".

Sharing a 'Folder', ala Syncthing, would be done by having a Data Store on each device that mutually own each other, and each of them contain their own local Data Stream and the Store on the other device.

From a User's perspective, the Data Store is what they should be operating on.

As an example, an "Email" data store could contain all Emails and attachments.

The intention is that you will be able to set "sync all my email locally" or "sync the last year of emails locally" or "sync the most recent X GB of email locally" or "index all emails for searching".

## Data Streams

Every piece of data that is stored goes into a Data Stream.
A Data Stream is unique to a user, which is unique to an instance.
Only that User on that instance can enter data into the Data Stream.

The Data Stream is defined by a Key Pair.

The Data Stream is cryptographically signed, using it's private key, for each entry.
The Signing includes the hash of the previous entry, so you can verify the entire history if the full Metadata is synced.

It is functionally append only, and can be synced in either direction.

## Blob Data Storage

`Data` that is stored in the system is shared globally on the instance.

The most efficient way to store the Data and dedupe it is to have a single Global Data Table that stores the Data in one place.
Data is referenced by its hash, so if user A and user B both are storing the same file we'd be able to dedupe and only store a single copy.

In the unlikely event that a user is able to guess a data hash they would be able to access any data stored in the global data table.
Though the user must have an account on the instance where the data is stored.

If you need more security than that for a data stream, it should be encrypted.

On the backend Eidetica is allowed to do any compression/chunking it wants so long as it can reconstruct the file.
Things like grouping entries for S3 backups, transparent compression, deduping at a block level, etc.

## Plugin Interaction

A plugin is effectively a view into a data store.
e.g. You would mark an Email plugin to manage the data for your Email data store.

TBD if multiple plugins will be allowed for a single Data Store.
