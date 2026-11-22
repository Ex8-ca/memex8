# memex8

A self-hosted memory for AI agents. It stores what an agent is told, lets older memories fade at a rate that depends on what they are, groups related memories, and consolidates scattered fragments into summaries.

## Language

### What is stored

**Memory**:
One unit of remembered content with its tags, source, type and visibility.
_Avoid_: Note, entry, record, document

**Memory type**:
The kind of a memory (profile, preference, fact, event, ...). It sets how fast the memory fades. A type the system does not know fades like `general`.
_Avoid_: Category, kind

**Realm**:
A named group of related memories, assigned automatically or by hint. A realm is discovered, not designed.
_Avoid_: Topic, folder, namespace, cluster

**Visibility**:
Whether a memory is `private` (default) or `public`. Anything not clearly `public` is private.
_Avoid_: Privacy level, sharing flag

**Certified memory**:
A memory the user explicitly confirmed. Maintenance never archives, merges or rewrites it.
_Avoid_: Pinned, locked, verified (verification is a separate rating)

**Dream**:
A speculative recombination of memories, kept apart from memories. It becomes a memory only when the user chooses to keep it.
_Avoid_: Insight, suggestion

**Evidence**:
The captured source text a memory was derived from, kept in a ledger and linked from the memory.
_Avoid_: Attachment, citation

### Privacy

**Private span**:
Text between `<private>` and `</private>` tags. It is removed before anything is stored, hashed or sent to a model, and replaced by `[private]`.
_Avoid_: Secret, redacted block

**Erasure**:
Permanently removing a memory, including every derived copy of it.
_Avoid_: Delete, purge

**Maintenance gate**:
The guard that keeps an erasure from committing between fetching a memory and finishing the response that contains it.
_Avoid_: Lock, mutex

### Upkeep

**Slumber**:
The scheduled maintenance pass: reorganize realms, link associations, consolidate, rebuild indexes.
_Avoid_: Cron job, background job, nightly run

**Pause** (of slumber):
An operator's standing instruction that slumber must not run, kept until resumed.
_Avoid_: Disable, stop

**Consolidation**:
Replacing a group of related memories with one summary memory, keeping the sources recoverable.
_Avoid_: Compaction, merging

**Association**:
A weighted link between two related memories.
_Avoid_: Edge, relation, connection

**Compressed index**:
A smaller copy of every memory's vector that makes search faster. It is only a cache: when in doubt, search uses the full store.
_Avoid_: TurboVec index, vector cache
