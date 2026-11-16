# Repo audit remediation plan

**Scope:** Correct confirmed audit failures in isolated local branches, integrate with regression evidence, and stop before push, PR, deployment, production-memory changes, or deletion. All tests use disposable stores and synthetic fixtures.

## Scheduling and clustering
- Replace duration-based approximations of five-field cron with wall-clock scheduling; reject zero/invalid steps, reject ambiguous mixed-name/numeric weekdays and bare weekday steps with actionable errors, validate before `serve` binds, and coalesce missed ticks. Keep idle triggers. Test midnight, hourly, invalid expressions, and pause/resume interaction.
- Recompute k-means centroids per component. Use angular clustering fixtures and re-enable the previously skipped test in CI.

## Plugin privacy and checkpoint integrity
- Make required checkpoints fail closed on backend/receipt failure. Preserve normalized transcript evidence including tool and structured messages, redact private spans before chunking, and honor auto-sync consent.
- Bound chunks to the embedding input limit; frame and hash archive pages for checked reassembly. Do not auto-certify archives. Keep checkpoint provenance as `memory_type=artifact` (a long-lived working archive, approximately 273 days to the default prune score; not permanent).

## HTTP, MCP, and ingestion boundaries
- Require auth for root webhook aliases. Complete the MCP SSE handshake and message lifecycle, with bounded request/session resources. Keep only the documented health API endpoints public.
- Report truthful partial/failed ingest receipts in REST, CLI, and MCP. Bound directory traversal and reject symlink escapes. Test negative controls against disposable services.

## Derived state and slumber controls
- Invalidate compressed and lexical indexes after corpus mutations. Rebuild after final slumber mutations, verify persisted index identity/content and file-pair integrity at load, and surface rebuild failures. Test edit/ingest/consolidation/restart and a non-default TurboVec bit width.
- Persist and enforce slumber pause/resume; reset running state after task failure. Remove dangling graph references on delete/undo, resume interrupted consolidation source deletion, and refuse archiving certified memories. Keep explicit user deletion semantics.

## CI, review, and release
- Run corrected Rust tests and all applicable Python lifecycle/plugin suites; run failpoint tests against a debug binary. Keep existing clippy debt explicit rather than claiming lint-clean.
- Build and smoke-test release images with disposable Qdrant over gRPC and the production OpenAI embedding configuration (dummy key for startup-only health). Gate publishing on successful tests and smoke health; do not trigger publication during remediation.
- Independently review the integrated diff for contracts, security/privacy, release viability, and public-repo leaks. Record actual test results and remaining caveats in the accompanying change summary.

## Rollback and boundaries

This branch has no production writes. Discarding the isolated branch rolls back the local changes. Shipping, deploying, and deleting stale containers each require a separate explicit checkpoint. The Docker image release workflow cannot be fully verified until a real Actions run.
