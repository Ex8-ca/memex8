# Evidence Erasure Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Permit explicit deletion of an evidence-linked memory without leaving active source text or allowing an idempotent retry to recreate it; do not claim historical backups are erased.

**Architecture:** Serialize deletion against capture using the existing maintenance guard. Persist a minimal deletion intent/tombstone in SQLite before deleting the Qdrant projection, then erase the source row and mark the tombstone complete. Reconcile interrupted deletes before accepting new capture or serving a source. Preserve idempotency keys as non-sensitive hashes/opaque keys only; do not persist the deleted request/content. Treat backup archives and Qdrant historical WAL/snapshots as a separate retention/crypto-erasure design gate, not solved by an API DELETE.

**Tech Stack:** Rust, rusqlite, Qdrant, Axum, disposable Qdrant integration tests.

---

### Task 1: Test ledger erase and replay barrier
**Files:** `src/storage/evidence.rs`, `tests/evidence_store.rs`.
1. Add failing tests for prepared-only and projected source erasure: no `record`, `request`, or content survives in current SQLite rows; repeated idempotency key is rejected; repeated delete is idempotent; an interrupted intent is visible after reopen.
2. Run `cargo test --release --test evidence_store`; require a test failure before implementation.
3. Add schema-versioned erasure intents with migration from v1, key digest and memory/source UUID, no source text; methods to begin/finish/reconcile and to deny tombstoned `prepare`.
4. Rerun the test; commit only related files.

### Task 2: Serialize and reconcile cross-store deletion
**Files:** `src/engine/evidence.rs`, `src/engine/mod.rs`, `src/storage/qdrant.rs`, `src/api/routes/memories.rs`, `tests/evidence_api.rs`.
1. Write a failing disposable-Qdrant test for authenticated create→delete→read 404→same-key retry rejection, prepared-only deletion, repeated deletion and a simulated interruption after intent creation.
2. Require the test to fail before implementation.
3. Under the maintenance guard, inspect both stores, begin intent, delete the projection with Qdrant `wait=true`, remove source text and complete intent. Expose a checked internal Qdrant delete that cannot be called casually to bypass the ledger. Reconcile unfinished intents at startup or before reads/capture; never return erased source text. Keep edit and other mutation refusal gates.
4. Rerun the integration test and store tests; commit related files.

### Task 3: Backup, restore, and residual-data gate
**Status:** restore preflight, keyed-archive validation, legacy-archive refusal and residual-byte documentation are implemented locally; no stopped-writer paired-volume restore drill has been run.
**Files:** `src/engine/backup.rs`, `src/engine/backup_validation.rs`, `tests/evidence_snapshot.rs`, `docs/EVIDENCE.md`.
1. Test that a logical backup after deletion cannot restore the erased source or memory, but retains the replay barrier; a pre-erasure archive cannot be mistaken for a post-erasure archive.
2. Implement manifest/schema compatibility and a restore-time refusal or explicit operator action for older archives capable of resurrection. Do not delete old backups automatically.
3. Document that SQLite pages/WAL, Qdrant WAL/snapshots and old archives can retain bytes; physical erasure needs a separately tested retention/cryptographic-key strategy. Do not enable production evidence until that strategy and paired-volume restore drill pass.
4. Run formatting, filtered Rust suite, integration suites and disposable-backend restore tests; review against the stated safety claims; commit.

**Release gate:** No production deployment or feature activation during this implementation. A PR and hosted CI are separate from local tests; only publish after review. Do not silently discard existing data or rewrite the operator Compose worktree.
