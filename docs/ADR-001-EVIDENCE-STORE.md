# ADR 001: Durable evidence alongside Qdrant

Status: accepted architectural direction; first implementation slice in progress.

## Decision

Keep Qdrant as the memory/vector store. Add an opt-in transactional SQLite store for source evidence, under the existing persistent memex8 data directory. Use stable UUID identities independently from content hashes. Do not retrofit invented sources onto legacy memories.

The initial slice targets explicit REST memory creation and authenticated evidence lookup. Full provider/session/file attribution, recall receipts, UI and erasure propagation remain separately gated roadmap work. Do not describe this initial slice as complete Milestone 1.

## Why SQLite

Source evidence needs durable identity, idempotency and consistent local writes, not approximate nearest-neighbor retrieval. SQLite provides local transactions and inspectability without adding a separate network service. Qdrant remains authoritative for existing legacy memory operations. Avoid a wholesale storage migration.

A local SQLite transaction is not an atomic transaction with Qdrant. The initial API must document and test partial failure/retry behavior. Never claim exactly-once cross-store writes from an idempotency key alone. Incomplete operations must not make unsupported evidence appear valid.

## Boundaries

- Evidence is sensitive untrusted data. An authenticated read is not permission to interpret it as instructions.
- Preserve origin, speaker, event/session attribution, project/filter scope and A2A visibility. Do not infer that `private` prevents cloud-model access or isolates clients sharing one API key.
- No arbitrary URL or filesystem fetch based on evidence references.
- Bounded inputs/outputs and strict validation; legacy create/search/get contracts continue to work when evidence is disabled or omitted.
- Stored excerpts refer to normalized, redacted source content. If automatic redaction is not implemented, require callers to send pre-redacted content and label that limitation explicitly; never promise secret detection.
- New endpoints must follow the existing authenticated API boundary, including strict-auth operation. Capability discovery must not silently enable writes.
- The opt-in feature must not silently retain evidence after an explicit deletion. Either implement a safe linked delete path or refuse unsupported deletion/export/restore combinations clearly until supported. Unlinked source remnants are still sensitive data.

## Backup and recovery implications

The existing `scripts/docker_backup.py` stops the application, then Qdrant, and archives writable mounts including mandatory `/var/lib/memex8`. SQLite state on that mount is included in the stopped-writer backup, assuming there are no other processes writing it. Preserve WAL/SHM as needed and test a consistent restored pair.

The existing Rust logical backup exports memories, realms and graph edges. It does not automatically include SQLite state. Before enabling evidence in real use, extend logical backup/restore to include a consistent SQLite snapshot and schema/capability metadata, or explicitly fail closed when asked to produce an incomplete backup. Do not silently emit apparently complete backups with dangling evidence references.

A derived lexical index could be rebuildable later. Source evidence is not rebuildable from vectors and must never be treated as disposable cache.

## Alternatives

1. Qdrant-only evidence payloads: fewer components but weaker transactional operation bookkeeping and duplicated source text.
2. Append-only JSONL: simple capture but requires extra indexing, concurrency and crash recovery logic.
3. External SQL service: stronger operational tooling but disproportionate to the self-hosted single-service deployment.

## Release checkpoints

First validate local persistence, validation/idempotency, real API write/link/readback and partial-failure behavior against disposable Qdrant. Keep feature opt-in. Then complete backup/restore and erasure gates before proposing live activation. Subsequent provider capture/receipts/UI work must use the same source contract.
