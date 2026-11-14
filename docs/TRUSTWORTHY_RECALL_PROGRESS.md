# Trustworthy recall implementation progress

Status: Milestone 0 safety code and review follow-up fixes are locally verified. The first opt-in M1 explicit REST evidence slice is locally verified; full M1 and Milestones 2–6 are not shipped. Release/live acceptance gates remain open.
Source baseline: `a10528d59edcf2e21ab7e3a5048702755dcdcd67`.

## Scope

Local implementation and testing only. No deployment, live migration, remote-host access, credential replacement or production-memory writes. Preserve the pre-existing `docker-compose.yml` changes.

## Baseline observations

- Engine package version: `0.1.0`; Hermes plugin manifest: `1.2.0`.
- This checkout is shallow and contains no local tags. Absence of local tags does not establish that no published tags exist. Keep component versions unchanged until release history is reconciled.
- Rust toolchain: cargo `1.99.1`, rustc `1.99.1`.
- Initial `cargo test -- --skip test_kmeans_simple` stopped at linking because the checkout forces `-lopenblas` and this host lacks OpenBLAS.
- For local test execution, a real OpenBLAS `0.3.34-1` package was retrieved from the official Arch package mirror and extracted into `/home/marc/.hermes/cache/scratch/memex8-openblas`. No system packages were changed. Supply its `usr/lib` directory through `LIBRARY_PATH` and `LD_LIBRARY_PATH`. This scratch dependency is not a deployment artifact.

## Python baseline and fixture correction

- Shared bridge: initially 13 passed / 1 failed because its stdio subprocess inherited the operator's LAN `MEMEX8_BASE_URL`. The test intentionally exercises a local handshake and must not depend on a live service.
- Corrected only that subprocess fixture to explicitly use `http://localhost:8080`, retaining its synthetic API key. Rerun: 14 tests passed.
- Backup workflow: 7 tests passed. These are fixture-driven workflow tests, not a real production restore drill.
- Visibility: 9 tests passed.
- Export/import path and provider availability: 13 tests passed.
- Cron filter: 10 cases passed.

## Implemented safety checkpoint

- Consolidation is now opt-in (`[slumber.consolidation] enabled = true`) and additive: it never deletes its source memories, including generation, embedding and storage failures.
- Groups preserve realm identity and visibility. Tagged records, including project records, are conservatively skipped. Existing summaries cannot become recursive inputs.
- Deterministic input-snapshot IDs prevent repeat summaries. `consolidation-input:<id>` tags identify exactly the selected bounded source set; these are interim lineage, not Milestone 1 evidence receipts.
- Prefetch uses `POST /api/v1/memories/search` with query, limit and minimum score. Results are bound to query/session/generation and invalidated across lifecycle changes.
- Speculative recall has a maximum of two inflight workers, no backlog, and a configurable `recall_deadline_ms` defaulting to 1500. Overload skips speculative recall; explicit tools are unaffected.
- Foreground waiting/publication is deadline-bounded. Requests cannot forcibly cancel a trickling response; lingering network workers remain capped at two.
- CI now includes the prefetch regressions and serial consolidation tests against a disposable Qdrant service. Hosted CI has not yet been run for these edits.

## Parent verification

- Combined Python invocation: **60 passed, 10 subtests passed**.
- Rust `cargo test -- --skip test_kmeans_simple`: **51 library + 51 binary + 38 integration test executions passed**. Library/binary execute duplicate module tests; do not represent these as 140 unique behaviors. Nine Qdrant tests are ignored in each default module run.
- Fresh local Qdrant `v1.17.0`, loopback-only ephemeral host port: **10 consolidation tests passed**, including all nine normally ignored storage tests. Readback assertions exercise preserved originals and summary payloads. Container stopped/removed afterward.
- `cargo fmt -- --check` and `git diff --check`: passed.
- Subagent reports `cargo build` passed and the unfiltered suite failed at the existing nondeterministic `test_kmeans_simple`; the parent independently reran the CI-equivalent filtered suite.

## Review follow-up and first M1 slice

- Independent review reproduced an old-query/new-client lifecycle race, synchronous setup/config exceptions, and malformed response crashes. Added regression tests first, then fixed all three. Parent reran `uv run --with pytest --with requests python -m pytest tests integrations/shared-memory/test_bridge.py -q`: **65 passed, 10 subtests passed**; standalone visibility suite: **9 passed**.
- The prefetch suite now has 31 cases. The implementation child also repeated the lifecycle race 20 times without failure; parent verification above is a separate combined-suite run.
- Split Python invocation is intentional: unrestricted root pytest collection imports the plugin package without Hermes and encounters the existing `No module named agent` issue. This is not represented as a passing root collection.
- M1 adds opt-in explicit REST source capture into SQLite and schema-versioned Qdrant evidence links; see [evidence usage and limitations](EVIDENCE.md) and [storage ADR](ADR-001-EVIDENCE-STORE.md).
- Parent reran filtered Rust suite: 51 library and 51 binary executions, 38 existing integration tests, and 4 evidence-store tests passed. Nine consolidation tests per module and the evidence API scenario are ignored in the default run.
- Parent separately ran the evidence API scenario against a fresh disposable loopback Qdrant plus temporary SQLite: **1 passed**. It covers authentication, create/link/readback, idempotency/conflict, failed embedding retry, legacy creation and refusal gates. Container removed afterward.
- CI now runs evidence API acceptance on a separate Qdrant service from consolidation tests to avoid embedding-dimension interference. Hosted CI is not yet executed.
- Evidence remains experimental: no source erasure, logical paired backup/restore, automatic chat attribution, receipts or UI. Unsupported lifecycle operations explicitly refuse. No production activation, commit or tag.

## Remaining gates and limitations

- Live authentication remains unverified after the earlier tool returned 401. No production keys were inspected or repaired.
- Read-only `git ls-remote --tags origin` found engine tag `v0.2.0` (peeled commit `e0469b142056bb3ce9cf3e0ebd742b91b3daac84`). The local Cargo manifest remains `0.1.0`; Hermes plugin is independently versioned `1.2.0`. Reconcile stale engine metadata against published release contents before assigning the next engine version. No new tag or version bump yet.
- No production backup/restore drill, authenticated live retrieval benchmark or cross-client deployment acceptance was performed.
- Consolidation safety does not protect every other legacy maintenance phase. Transactional mutation protection, evidence erasure propagation, reviewed apply/rollback and full source evidence are later milestones.
- No claim that Milestone 0 as a whole, or the complete roadmap, is finished. The two immediate safety implementation tasks are verified; remaining operational gates stay open.
