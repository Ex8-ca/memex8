# PR 57 integration verification

This report supersedes old-baseline implementation descriptions where they differ.

## Integrated base

Merged `origin/main` at `148c1d7` into checkpoint `e742b7b` in an isolated worktree. Preserved upstream engine version 0.2.0, plugin 1.2.0, dreams, timeline, lexical retrieval, compact search, certification, dedup recovery and structured consolidation provenance/recovery journals.

The old checkpoint's tag-based summary identity and legacy additive implementation are not resurrected. Upstream `preserve_originals` remains configurable; additive-source regression tests explicitly enable it. Existing recovery-backed archival behavior remains supported when it is false. Colon-scoped tags are conservatively excluded from consolidation; ordinary descriptive tags still work.

## Evidence restrictions

Explicit REST evidence remains opt-in and experimental. Protection now includes upstream unconsolidation, duplicate restoration, realm merge/split, certification, snapshot mutation/restoration and batch payload updates. Incoming imports cannot forge evidence links. Capture shares the maintenance lock. Automatic source erasure, logical paired restore, provider attribution, receipts and UI remain out of scope. Do not activate evidence against production before those gates are met.

## Local verification on reconciled tree

Parent independently executed:

- Full root Python backend/offline suite with the merged debug binary and disposable local Qdrant fixtures: **97 passed, 12 subtests passed**.
- Rust CI-equivalent `cargo test -- --skip test_kmeans_simple`: **162 library and 162 binary executions** (duplicate module suites), **4 evidence-store** and **38 existing integration** tests passed. Nine backend tests per module and one evidence API scenario remain ignored by default.
- Rust formatting passed. Merge-relative whitespace warnings in old upstream planning markdown are pre-existing; PR-relative checks should be evaluated against integrated main.

Implementation children additionally executed:

- Separate fresh Qdrant evidence API: 1 passed.
- Separate fresh Qdrant consolidation regressions: 9 passed.
- All nine plugin standalone suites passed.
- Benchmark/script unit suites: 29 passed.
- Merged plugin prefetch cases: 32 passed, including upstream filter plus deadline transport regression.
- Merged cargo build passed.

CI preserves upstream lifecycle tests and adds separate 3-dimensional consolidation and 8-dimensional evidence services. GitHub CI results must be checked separately before merge. No production service access, data migration, credential changes, deployment or release tag.

Original checkout's operator-local Compose edits remain excluded; tracked upstream Compose changes enter only through the main merge.
