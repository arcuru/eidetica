# Plugins

unimplemented!();

While the core of this tool will be the sync database and searching, plugins will be necessary for handling and interpreting the data.

e.g. Email. The email text (or email + its own metadata) will be stored, but Eidetica core won't know how to interpret it.
An email plugin will know how to format it for search, and potentially handle converting some input format as well.
It will also handle display.

My list of key plugins that I have kept in mind during the design phase.

- Email
- Chat
- Atuin (cmd line history)
- Files (full file sync)
- Web Browsing history
- Custom Application data

Perhaps not in the first version of plugins, but the intention is to allow people to run separate plugins outside of Eidetica with an API.
So you could write your own plugin and use it externally to the Eidetica project.

TBD on what the interface needs to look like.

## Searching

Plugins will need to have some interface for helping with searching, as they are necessary to interpret the stored data.
They will be called to translate the data into something that can be searched.

The interface here is also very TBD.

## Syncing

The plan is for plugins to sit on top of the Data Store and operate on it.
They store things in a standard format that is safe and efficient to sync.

As a consequence, sync nodes that don't need to do anything with the data don't need to know which plugins even apply.
