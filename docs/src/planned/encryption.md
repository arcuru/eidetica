# Encryption

unimplemented!();

End-to-End encryption should be relatively easy to implement from a technical level.
Similar to how Syncthing does it, we'll also be able to support having some nodes unable to see the data at all.

The idea is to implement a layer inside the DataStore that transparently handles the encryption for plugins.
Encryption would be a setting on the DataStore itself, and the plugin wouldn't have to care about it at all.
The data and metadata would be synced as with everything else, but only nodes where you explicitly entered the decryption key would be able to decrypt the Metadata or the Data.
