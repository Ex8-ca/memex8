# Evidence paired recovery checkpoint

Status: implemented and independently verified locally on `feat/evidence-paired-backup`, based on merged PR57 (`da8ffd5`). Not committed/published or production activated.

## Acceptance contract

1. A logical archive includes Qdrant memories/vectors, realms, graph and existing recovery journals plus a consistent SQLite evidence snapshot.
2. SQLite snapshot uses the backup API so committed WAL data and prepared idempotency records are retained. Copying only a live main database file is not sufficient.
3. A versioned manifest identifies required components and integrity digests. Digests detect corruption, not malicious replacement: restore only trusted archives.
4. Validate archive entries, JSON/schema versions, SQLite integrity and all evidence links before the first destination write. Missing/corrupt/unsupported evidence must fail without a partially populated target.
5. Prove recovery by creating evidence through authenticated HTTP, backing up, restoring into separate empty Qdrant and SQLite storage, then reading the exact memory/source IDs and content back through the authenticated API.
6. Preserve legacy archives when they have no evidence dependencies. Never silently accept dangling links.
7. Reject unsupported nonempty/merge restore; `--force` must not bypass structural safety.
8. Cross-store restore is not atomic. Document partial-write behavior and recovery/retry requirements explicitly; errors must not be reported as successful restore.
9. Generic import/export, destructive mutation, slumber and evidence erasure remain blocked unless separately implemented and verified. Backup support does not imply those paths are safe.

## Test matrix

- Exact memory/source UUID and speaker/scope/visibility preservation.
- Committed WAL content and failed-projection pending idempotency preservation.
- Missing SQLite component, corrupt SQLite bytes and unsupported schema refusal.
- Invalid/foreign evidence references and inconsistent scopes/spans refusal.
- No destination changes after validation rejection.
- Nonempty destination refusal even with force.
- Safe archive extraction: no traversal, links or unexpected authoritative entries.
- Legacy no-evidence round trip and existing journal behavior.
- Interrupted writes reported honestly; repeat behavior tested or explicitly refused.

## Operational limits

Use only disposable local backends for this checkpoint. No production credentials, SSH, live memory reads/writes, deployment or default evidence activation. The operator's original Compose overrides remain untouched in the main checkout.

A logical restore test does not replace a stopped-writer paired volume restore drill. Source erasure and prevention of resurrection from old backups require a separate design/checkpoint before evidence can be activated for real sensitive data.

## Verified behavior and operator requirements

Paired backup requires `MEMEX8_BACKUP_WRITERS_STOPPED=1`. Stop API, CLI, scheduler and direct-store writers before running the backup process. This is an explicit operator attestation, not enforcement against external writers; concurrent globally consistent snapshots are unsupported. The maintenance lock alone does not establish that guarantee.

Extraction rejects unexpected entries and links, with a 512 MiB per-component and 1 GiB total uncompressed limit. Manifest hashes detect corruption, not adversarial rewriting. Realm, graph and journal identity/vector semantics are validated before destination writes.

Paired restore creates `evidence.restore-in-progress` beside the evidence database before writing. On a write-phase failure, **discard both partial destination stores**. Remove the marker only after discarding both; never clear it merely to retry over partial data. The marker prevents accidental retry; it is not a power-loss-atomic cross-store transaction.

Parent verification on the final hardened tree:

- Filtered Rust suite: **171 library + 171 binary executions** (duplicate module suites), **1 snapshot + 4 evidence-store + 38 existing integration** tests passed. Nine backend tests per module and one evidence API scenario remain ignored in default runs; known kmeans test excluded.
- Separate fresh source/restore Qdrant v1.17.0 plus temporary SQLite acceptance: **1 passed**, including exact source readback, realm/graph/journal recovery, malformed-archive refusal, real TCP transport interruption after SQLite installation, retry-marker refusal and fresh-target recovery. Containers removed afterward.
- CI configuration includes a distinct restore backend. Hosted CI has not run for this branch.

This checkpoint fixes graph point IDs/vector shape/application waiting discovered by the recovery drill. It does not implement erasure, generic import/export, source revisioning or safe evidence slumber. Evidence-linked recovery journals remain explicitly refused. No production paired-volume restore drill was performed.
