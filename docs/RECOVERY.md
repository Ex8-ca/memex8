# Recovering a collection that ignores writes

Symptom: `memex8 doctor` reports "N of M sampled memories ignore writes", or the server log says
"Storage write check: ... ignore writes". Edits, deletes and access updates to those memories report
success and change nothing. Search and reads still work, which is why nothing else notices.

This was seen once on a live corpus (98% of points affected). Qdrant answered `completed` to updates
it did not apply. The likely cause is that those points carry an internal version above the
collection's current operation counter, so Qdrant treats newer writes as outdated. That is a guess;
the fix below does not depend on it.

## Fix: rebuild the collection

New points are healthy, so copy every point into a freshly created collection.

1. Take a backup first: `memex8 backup <dir>`, and optionally a Qdrant snapshot
   (`POST /collections/memories/snapshots`).
2. Stop the memex8 server so nothing writes during the copy.
3. Scroll every point with its vector and payload and save it to a file.
4. Create a scratch collection with the same vector size, distance and payload indexes
   (`source_hash`, `related_memory_ids`, `memory_type`, `verification_status` as keyword; `ingested_at`
   as datetime), and upsert every point into it. Read it back and confirm every id, payload and vector
   is identical.
5. Delete `memories`, recreate it with the same settings and indexes, and upsert the points back.
   Confirm again.
6. Run `memex8 doctor`: it should report every write applied. Start the server.

Only the `memories` collection needed this; the other collections were healthy.
