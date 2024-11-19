# Data Organization

unimplemented!();

There are assumed to be several levels of organization in an Eidetica node.

## Global

An Eidetica node consists of 1 or more Users.

Global data is limited, but will include settings for the node in general and a global Data Table.

## Users

A user on an Eidetica node will have their own set of settings and preferences, as well as their own set of "Data Stores".
All data goes into one of the Data Stores.

### Data Stores

Data stored in Eidetica is stored in "Data Stores", that are groups of related data managed together.
You might prefer to call them "Folders".

A "Data Store" may contain different types of data, but should be logically grouped.
Names are from the user.

As an example, an "Email" data store could contain all Emails and attachments.

The intention is that you will be able to set "sync all my email locally" or "sync the last year of emails locally" or "sync the most recent X GB of email locally" or "index all emails for searching".

Each "Data Store" will have its own table/set of tables for sharding and efficient syncing.

### Blob Data Storage

`Data` that is stored in the system may be shared globally depending on user preference.

The most efficient way to store the Data and dedupe it is to have a single Global Data Table that stores the Data in one place.
Data is referenced by its hash, so if user A and user B both are storing the same file we'd be able to dedupe and only store a single copy.

This is not preferred in every case for security purposes, but should be encouraged by Node admins and preferred by users without excess security concerns.
In the unlikely event that a user is able to guess a data hash they would be able to access any data stored in the global data table.

Yes, users concerned about security could always use Encryption along with a global data table, but Encryption has its own set of problems.

Because there are so many things that go into it, building some flexibility into the system is the right way to do it until a model is settled on.

## Plugin Interaction

A plugin will exclusively be given access to a single data store.
e.g. You would mark an Email plugin to manage the data for your Email data store.

TBD if multiple plugins will be allowed for a single Data Store.
