# Data Model

The Eidetica data model is no more than an opinionated database.

- Everything is an `Entry`.
- Each `Entry` is identified by a Unique ID in UUIDv7 format.
- Each `Entry` contains two types of data, `Metadata` and `Data`.
- Users/Plugins can also set a `Parent` entry to indicate if the new `Entry` is replacing old data.
- `Old` entries are "Archived", everything else is "Active".

Eidetica will capture and expose additional pieces of info for every `Entry` that are necessary for syncing and other operations:

- A `Device ID` tracking where the `Entry` was created.
- The `Archived` or `Active` bit.
- Creation time.

## IDs

Entry IDs are stable and globally unique.
Storing a reference to Entry IDs is therefore safe to do.

Entry IDs are how everything should refer to Entries, both the Data and the Metadata.

## Metadata vs Data

Eidetica draws a distinction between Metadata and Data within an Entry. What's the difference?

### Metadata

- Stored directly in the underlying DB, and synced everywhere.
- JSON format.
- Ideally this should be as small as possible.
- Plugins have leeway to do whatever they want with this data, but with a Key/Value configuration Eidetica will handle searching by K/V.

### Data

- Blob storage from Eidetica's perspective.
- Store larger pieces of data here, and data that may be duplicated many times.

## Storage differences

Metadata is stored as a JSON blob inside the database, and (once Sync is implemented) will be Synced to all devices.
Metadata should therefore be relatively small, and contain just enough info to identify the Data blobs.

Data is stored by hash.
If 2 pieces of data have the same hash, we only store a single copy.

Data may not be synced to every device.

## Example Usage

The File plugin stores the file contents as `Data`, and the path/last modification time/etc as `Metadata`.

An email plugin may store the contents of an email as `Data` and might store the email header info as `MetaData`.
Attachments should likely be stored as a separate `Entry`, with the main email `Entry` containing a link to that Entrie's ID.
