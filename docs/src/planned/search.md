# Search

unimplemented!();

It's no use storing your personal data if you can't search it.

Searching and indexing is a relatively expensive operation, so we don't do it automatically.
Instead, the user will need to explicitly configure what data is indexed and searchable.

Plugins will need to have some role in this for interpreting the data, as the core of Eidetica is generic and won't know anything about what the data blobs are.

Eidetica will intentionally support both standard text searching and RAG.

## RAG (Retrieval-Augmented-Generation)

AI bots need access to data to answer questions.

You don't want to hand all your personal data to ChatGPT.

Therefore, if you store your data in Eidetica, we will expose it via an API and you can send all the info necessary for the AI to answer the question.

For example, ask an AI about your friend Cheryl and it currently can't help you, or it's Google made and it creeps on all your data within Google.

With Eidetica, you could in theory ask it about your friend Cheryl, in turn it would query Eidetica for info, and Eidetica could return your saved Contact Info, recent emails, calendar events, personal notes, etc that are relevant to Cheryl.

## Implementation

TBD on whether Search is done via a homegrown solution or shelling out to something like Elasticsearch.
