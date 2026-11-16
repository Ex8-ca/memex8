# Repo audit remediation — change summary / PR draft

## Summary

- Correct wall-clock cron evaluation and k-means centroid arithmetic; restore the clustering test to CI.
- Fail closed on required transcript checkpoints, preserve structured/tool evidence with checked chunk manifests, redact private spans before archiving, and respect automatic-sync consent.
- Authenticate webhook aliases; repair MCP SSE handshakes and bounded session/request handling; make the health-auth contract explicit.
- Keep search indexes coherent with corpus changes: invalidate on writes/deletes/undo, rebuild after final slumber mutations, prove persisted index identity/content at load, and surface rebuild failures. Honor the configured TurboVec bit width.
- Report partial and failed ingestion truthfully across REST, CLI, and MCP; bound directory traversal and reject unsafe symlinks.
- Persist and enforce slumber pause, resume interrupted consolidation deletions, prune graph references, and refuse archive/decay mutations on certified memories while retaining explicit deletion.
- Run formerly skipped/omitted suites in CI, with debug-only failpoint tests using the debug binary. Gate release-image publishing on a Qdrant-backed smoke test using the production OpenAI embedding configuration.

## Scope and decisions

This branch contains local code, tests, documentation, and CI changes only. It has **not** been pushed, opened as a PR, merged, released, deployed, or run against production memory. Checkpoint archives remain `memory_type=artifact` by product decision: approximately 273 days to the default prune score, **not** a permanent retention guarantee and not an instruction to certify every archive. The synthetic OpenAI key in the image smoke test only exercises startup and health, which do not request an embedding.

## Regression evidence

- Targeted bit-width regression: with a real disposable Qdrant and mock embeddings, set `[turbovec] bit_width=2`, store one memory, trigger slumber, inspect the persisted sidecar, then restart and load the index. The same test **fails on `65131aa`** (`4 != 2` in the sidecar) and **passes on this branch**. Full index-coherence suite: 6 tests passed.
- Previous integrated verification: Rust library tests 211 passed / 9 ignored; 14 Python lifecycle suites returned success after the control merge; independent reviewer ran Rust integration targets (38 passed) and 22 selected Python tests. These numbers predate the final release-workflow/bit-width regression additions; CI must rerun them on the final branch.
- Release smoke verified locally with disposable Qdrant over gRPC and the production OpenAI provider/model/dimensions using a synthetic key: `/api/v1/health` returned 200 `OK`; unauthenticated `/api/v1/stats` returned 401. The real GitHub Actions release job has **not** been run.

## Review notes / limitations

- Clippy remains informational (`continue-on-error`) for pre-existing lint debt; CI does not claim lint-clean.
- Cron weekday edge syntax is rejected rather than silently reinterpreted. The five-field parser translates all-numeric weekday fields from Unix numbering to the crate's numbering; all-named fields retain names. Mixed fields (`MON,3`) and weekday steps without an explicit range or wildcard (`5/2`, `MON/2`) are refused. Named steps (`MON-FRI/2`) are also refused because their expansion is crate-dependent; use numeric `1-5/2`. `serve` validates the schedule before building the engine so the API cannot appear healthy with a dead background scheduler. Existing configs using these ambiguous forms must be rewritten before upgrade.
- Queue-saturation, one-hour MCP session expiry, and tool-call cancellation have not been exercised end to end. Release publishing still needs an actual GitHub Actions run after explicit approval.

## Reviewer checklist

- Verify webhook aliases, MCP SSE routes, and health exemptions with authenticated and unauthenticated probes.
- Verify a checkpoint failure cannot report success or compress anyway, and that private spans never reach archive payloads.
- Verify after corpus mutation that neither in-memory nor persisted TurboVec/BM25 state hides a live memory.
- Verify consolidation interruption can resume and remains reversible, and certified memories cannot be archived by decay.
- Verify CI points failpoint tests at `target/debug/memex8`, not release, and release smoke connects to Qdrant gRPC `:6334` (HTTP `:6333` is health only).
