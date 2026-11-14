# memex8 Roadmap: Trustworthy Recall

Status: implementation underway. Milestone 0 consolidation/prefetch safety changes are locally verified; operational/release gates remain open. See [implementation progress](docs/TRUSTWORTHY_RECALL_PROGRESS.md). Milestones 1–6 are not shipped.

## Goal

Keep memex8's existing Rust/Qdrant/TurboVec architecture and Hermes/shared-agent integrations. Make recalled knowledge explainable, corrections historical, consolidation recoverable, and retrieval reliable. Ship independently useful checkpoints rather than one large rewrite.

This plan is based on local source review at commit `a10528d`, plus the hackathon design review. It is not a runtime audit of the deployed service. An unrelated local modification to `docker-compose.yml` exists and must be preserved.

## What exists and what needs work

| Area | Observed implementation | Planned change |
|---|---|---|
| Recall | Semantic search, importance/recency weighting, query intent, background prefetch | Query-correct, scoped, deadline-bounded recall; optional hybrid search |
| Evidence | `source_file`, `source_hash`; session extraction has optional `source_message_idx` | Persistent evidence references, exact excerpts, lineage, recall receipts |
| Verification | Status/confidence fields and LLM sweep | Evidence-backed transitions; separate plausibility from verification |
| Corrections | No explicit temporal/supersession model found in reviewed memory struct | Immutable revisions and current/historical retrieval |
| Slumber | Consolidation, verification, associations and maintenance | Proposal/apply/receipt/rollback workflow for meaning-changing operations |
| Scope | Project tags, private/public A2A visibility, shared MCP bridge | Preserve boundaries across every derived artifact and retrieval surface |
| Operations | Backup/recovery scripts and CI already exist | Extend them to all new state; restore drills and cross-client acceptance |

### Source findings that change the order

1. `src/engine/slumber.rs::llm_consolidate` limits the prompt to ten ranked memories and truncates their text, but derives `ids_to_delete` from the entire eligible realm. It attempts deletion before storing the summary. This is a source-level data-loss risk, not a claim that production has lost data.
2. Project-tagged records are deliberately excluded from that pass because it does not preserve project scope or attribution. Keep that exclusion until the replacement passes scope tests.
3. `plugins/memex8/__init__.py::queue_prefetch` accepts a query but currently calls `client.recall(top_k=...)` without it. Both queueing and reading can join a previous worker for three seconds. A complete query/session/generation contract is needed.
4. The verification sweep asks a model whether a claim is still true without supplying current external evidence. That is plausibility assessment, not verification of running services or personal facts.
5. The live memex8 search tool returned `401 Invalid API key` during research. Diagnose the authorized client/server configuration before live acceptance; do not print or replace secrets as part of this plan.
6. Release identifiers need reconciliation: `Cargo.toml` says `0.1.0`, while plugin/catalog descriptions refer to later versions. Assign actual release numbers only after checking tags, manifests and deployment state. Milestone labels below are not version claims.

## Operating rules

- Plan work and tests locally first. Deployment, new remote-host access, data migration and live imports require their own approved scope.
- Preserve embeddings, dimensions, API credentials, Compose overrides, and existing volumes during upgrades.
- Default new behavior to opt-in except fixes preventing destructive loss, scope leaks or false verification.
- Existing API responses remain valid; add optional fields and capability negotiation before requiring new inputs.
- Retrieved memories are untrusted data, never instructions or authorization. Evidence and receipts are sensitive too.
- A realm is an organizational cluster, not a permission boundary. Project filters are not authorization under a shared API key.
- `visibility=private` currently means ineligible for public A2A sharing, not hidden from cloud models or other authenticated clients.
- Never invent missing historical sources or dates. Legacy records remain explicitly unattributed/undated.
- Preserve source speaker: user statements, assistant inferences and tool observations are different evidence classes.
- Study designs, implement independently, and check licenses before copying code. In particular, Aletheia advertises AGPL-3.0; do not copy its implementation into this MIT project without a licensing decision.

## Milestone 0 — Stop unsafe loss and establish a baseline

Deliverable: a small safety/reliability checkpoint before schema expansion.

- [ ] Add regression tests reproducing the consolidation input/delete mismatch and delete-before-save failure path.
- [ ] Stop destructive deletion in the legacy consolidation path. Until reviewed apply exists, keep source records and write only an opt-in, scope/visibility-safe derived summary with input IDs. If boundaries cannot be preserved, skip the group.
- [ ] Do not describe duplicate retention as a permanent solution: prevent repeated generation of identical summaries and exclude recursive summary-of-summary inputs by default.
- [ ] Fix query-aware prefetch against the actual REST search contract; test unrelated query isolation and session switching before changing concurrency.
- [ ] Diagnose the 401 without exposing key material; record only configuration source, sanitized target and auth status. Local fixtures remain usable if live access is blocked.
- [ ] Capture baseline build/test results, representative retrieval results, latency and injected context sizes. Record existing failures separately.
- [ ] Reconcile engine/plugin/catalog versioning and create a changelog policy; do not silently renumber a published release.

Acceptance: failed summary generation/embedding/storage deletes zero sources; a group larger than ten loses zero unrepresented records; no cross-project/public-private merge; query A cannot receive query B's late result; no credential values in logs.

Primary files: `src/engine/slumber.rs`, `plugins/memex8/__init__.py`, plugin tests, Rust integration tests, release manifests.

## Milestone 1 — Evidence and recall receipts

Deliverable: answer “where did this memory come from?” through REST, Hermes and the shared MCP bridge.

### Data contract

- Add schema-versioned optional `evidence_refs` and `derived_from` to memory records. Evidence references identify a stable source record and message/span, not an arbitrary path the server may open.
- Source records carry origin type, agent/device/session, speaker role, capture time, normalized-content hash, scope and visibility. Keep original timestamps when importing.
- Store exact excerpts with explicit redaction metadata. Define span coordinates over the normalized stored text, not the original unredacted secret-bearing input.
- Evidence may point to an authorized external document; distinguish verified snapshots, mutable references, unavailable sources and legacy attribution.
- Preserve append-only source versions for ordinary updates, but support actual erasure for explicit deletion requests. “Append-only” is not an excuse to retain forbidden data.

### Tasks

- [ ] Choose a durable evidence/journal store in a short ADR. Preferred direction: transactional SQLite under the existing persistent memex8 data directory; Qdrant remains the search/payload store. Validate dependency, volume and backup costs first.
- [ ] Define stable source/event IDs, content revisions and idempotency keys; do not reuse mutable content hashes as identity.
- [ ] Thread source/message attribution through turn sync, session extraction, file ingestion, explicit tools, REST and the shared bridge.
- [ ] Return compact evidence metadata in search/get responses; fetch full authorized evidence on demand.
- [ ] Add an opt-in recall receipt: query/request ID, filters, memory revisions, evidence IDs, ranking components, exclusion reasons and duration. Full query text logging is off by default.
- [ ] Add a memory-detail evidence panel in `web-dist/index.html`; show unsupported legacy provenance honestly.
- [ ] Apply privacy, redaction, retention and erasure to sources, excerpts, receipts, exports and derived records. Never fetch arbitrary evidence URLs/paths on behalf of retrieved text.

Acceptance: new records from every supported ingestion path link to correct source roles; excerpts resolve to the right normalized source; legacy load/export/import works; unauthorized scopes cannot expose evidence through get-by-ID or receipts; deleting a source removes/redacts dependent sensitive excerpts and invalidates unsupported derivations without silent resurrection.

Primary files: `src/storage/qdrant.rs`, `src/storage/migrations.rs`, `src/engine/session.rs`, `src/engine/ingester.rs`, API route/models, `src/mcp/`, plugin, shared bridge, web UI, backup scripts.

## Milestone 2 — Corrections and time-aware recall

Deliverable: current answers exclude explicitly superseded facts; historical queries can reconstruct recorded beliefs.

### Temporal model

Use immutable fact revisions with a stable `fact_id` and explicit links between revisions. Separate **valid time** (when a claim applies) from **recorded time** (when memex8 learned it). A timestamp pair alone is not full bitemporal history.

Proposed fields: `fact_id`, `revision_id`, `supersedes_revision_id`, `valid_from`, `valid_to`, `recorded_at`, plus journal events recording acceptance/retirement. Define UTC RFC3339 timestamps and half-open validity intervals `[from, to)`.

- [ ] Add an explicit correction operation with expected-revision checks and idempotency keys. Reject cycles, self-links, unauthorized references and incompatible scopes.
- [ ] Default normal recall to current accepted revisions; retain historical records outside default results. Legacy undated records remain eligible but must not masquerade as dated evidence.
- [ ] Support separate `valid_at` and `known_at` query parameters so late-arriving historical corrections do not rewrite what the system previously knew.
- [ ] Close predecessor validity at the accepted successor's effective date, not blindly at ingestion time. Reject invalid intervals and define unresolved overlapping/conflicting revisions.
- [ ] Automatic contradiction detection creates a proposal, not an accepted correction. A newer sentence or higher similarity score alone cannot supersede a fact.
- [ ] Keep verification status independent: a fact can be historically verified yet currently superseded.
- [ ] Expose history and corrections in REST, MCP/Hermes tools and memory detail UI.

Acceptance: current/historical answers differ correctly; late imports preserve both clocks; concurrent corrections yield one winner or an explicit conflict; duplicate retries create one correction; unknown effective dates remain unknown; legacy records survive migration; no A2A disclosure of private predecessor text.

## Milestone 3 — Evidence-backed verification

Deliverable: “verified,” “contradicted,” “stale,” and “unverified” have enforceable meanings.

- [ ] Define a pure, exhaustively tested transition function over prior evidence and new check outcomes.
- [ ] Store verification attempts separately from conclusions: method, target, checked revision, observation time, expiry, result and evidence reference.
- [ ] LLM plausibility may flag review candidates but cannot by itself produce verified/contradicted states.
- [ ] Start with registered, operator-approved deterministic checks. Safe initial checks: confined file existence/hash and allowlisted HTTP status/JSON-field observations.
- [ ] Do not ship arbitrary shell checks or unrestricted URL/file probes. Bound time/size, redirects, DNS/IP targets and permitted filesystem roots to prevent SSRF or local-file disclosure.
- [ ] Checks run only on the explicitly selected execution host. A server-side path check cannot establish the state of a desktop client.
- [ ] Timeouts/401/429/5xx/malformed responses are operational failures, not proof the remembered claim is false. A transient error must not clear an existing contradiction.
- [ ] Require evidence plus explicit acceptance to resolve a contradiction; personal facts require user/source evidence, not infrastructure probes.
- [ ] Make status/evidence visible through search explanations and the existing verification summary.

Acceptance: table-driven transitions cover every prior state and outcome; stale revisions cannot receive fresh verification; auth/network failures never create false contradictions; a contradicted claim remains contradicted through an outage; malicious probe targets are refused.

## Milestone 4 — Reviewable and reversible slumber

Deliverable: stage → review → apply → receipt → compensating rollback.

- [ ] Introduce proposal types for consolidation, correction, retirement and scope-sensitive realm changes. Index optimization can remain automatic if it cannot change knowledge or boundaries.
- [ ] Each proposal records immutable input revisions, scope/visibility, evidence, output, model/config identifiers when used, and an expected-state fingerprint.
- [ ] Partition inputs by compatible scope/visibility before generation. Derived content inherits the most restrictive permitted disclosure; never promote private input to public output.
- [ ] Review UI shows before/after text and evidence; support accept, reject and defer. Keep auto-apply disabled for meaning-changing operations initially.
- [ ] Use a durable transactional operation journal. Qdrant and the journal are not one transaction: implement replayable prepare/apply/commit states, idempotent projection updates and recovery after partial failure.
- [ ] Single-writer serialization and expected-revision checks must cover every mutation path, including REST edits, ingest, scheduler, imports and slumber. Never claim atomicity from a best-effort read-then-write comparison.
- [ ] Commit a replacement before retiring source revisions, verify it by readback, and retain recovery information. Original evidence is not deleted by consolidation.
- [ ] Rollback is a new compensating operation: revert only unchanged affected revisions, preserve later unrelated writes, refuse conflicts, and never resurrect explicitly erased data.
- [ ] Add run summaries, pause/kill switch and operator-visible incomplete/recovery states.

Acceptance: crash injection at every journal/projection step recovers deterministically; repeated apply is idempotent; concurrent manual edit causes a conflict; rejected/deferred proposals do not alter accepted knowledge; rollback restores targeted state without reverting unrelated work; derived outputs retain source access restrictions.

## Milestone 5 — Hybrid retrieval and bounded recall

Deliverable: measured improvement for exact identifiers without regressing semantic retrieval or chat responsiveness.

- [ ] Spike lexical indexing against the actual pinned Qdrant version and evaluate native facilities versus a rebuildable SQLite FTS5 projection. Do not assume support from a latest-version example.
- [ ] Combine dense and lexical candidates with reciprocal-rank fusion initially; exact identifiers receive explicit coverage tests. Do not directly mix incomparable raw score scales.
- [ ] Apply scope, visibility, current/historical selection and deletion filters to both retrieval arms before fusion. Define minimum-evidence/relevance gates independently from RRF scores.
- [ ] Preserve existing importance/decay/intent behavior as an explainable bounded reranking stage; benchmark against a fixed dense-only baseline.
- [ ] Use one configurable end-to-end prefetch budget, initially targeting 1,500 ms. This is a target to measure, not an existing guarantee. No extra blocking joins outside it.
- [ ] Associate results with query, session and generation. Limit inflight work, discard late results and coalesce obsolete requests; ensure waiting less does not leave unbounded network workers running.
- [ ] Use a bounded writer queue with durable acknowledgement semantics, selective retries and explicit overload reporting. Never silently drop explicit user saves; visible failure or a durable pending record is required.
- [ ] Expose sanitized auth/degraded/timeout/queue status and bounded shutdown flush. No auth-error retry storms.
- [ ] Test session injection dedup across corrections and compression: a previously injected old revision must not hide its accepted replacement or disappear permanently after compaction.

Acceptance: exact paths/ports/package names are found by the lexical arm; unrelated questions can return no memory; semantic Recall@5/MRR does not regress beyond a predeclared tolerance; receipt filters match both arms; controlled slow-server tests bound prefetch return time within budget plus a documented scheduling tolerance; queue/thread counts remain bounded under stress.

## Milestone 6 — Safe historical backfill and optional companions

Deliverable: measured, resumable history import into memex8, not an unbounded replay into production.

- [ ] Evaluate Backfill in a disposable Hermes profile and memex8 store through the supported MemoryProvider lifecycle. Do not assume compatibility from its provider-agnostic claim.
- [ ] Support dry-run counts and filters, original event timestamps, persistent import IDs, compression-lineage dedup, resumability and completion verification.
- [ ] Respect redaction, no-save markers, tombstones and scope. Exclude cron/subagent content by default; assistant assertions must not become user facts.
- [ ] Measure a fixed set of recall questions before/after and report misses, wrong-source answers and stale answers as well as hits.
- [ ] Keep MemDiffs and Assumption Ledger as optional companion evaluations, not required backend dependencies.
- [ ] Evaluate CMI separately for context/cache behavior; do not bundle a context-engine replacement into a memory release.
- [ ] Consider route-aware disclosure only after defining gateway/platform/model/fallback trust propagation. Existing A2A private/public flags are not sufficient. Any future restriction must cover extraction, consolidation, tools and fallback models too.

Acceptance: repeat import creates no duplicate source events; interrupted import resumes; deleted/no-save content does not return; imported corrections retain historical dates; memex8-specific before/after evidence is recorded before production import is proposed.

## Test and release strategy

### Fixed regression corpus

Create small deterministic fixtures covering: changed endpoint, backdated correction, conflicting concurrent corrections, user preference vs assistant guess, identical service names across projects, mixed private/public inputs, exact identifier vs paraphrase, unrelated query, failed verifier, source deletion, interrupted apply, and repeated backfill. Use sanitized fixtures, not copied private chat history.

Measure retrieval independently from answer generation. Unit tests use controlled clocks and scripted model responses. Live model checks are separately labeled, bounded and never substituted for deterministic tests.

### Gate for every checkpoint

1. Write failing regression tests before implementing each behavior.
2. Run Rust formatting/tests/build and relevant Python plugin/shared-bridge tests; capture actual results. Use `.github/workflows/ci.yml` as the existing baseline, not stale TODO claims.
3. Add isolated Qdrant-backed tests to CI for storage/API behavior. Document existing informational Clippy and skipped nondeterministic test limitations; do not claim a stricter gate already exists.
4. Exercise the API and affected clients against a disposable store; verify writes by reading exact IDs/revisions back.
5. Test forward migration, rerun/idempotency, interrupted migration, full backup and isolated restore. New journal/evidence/index state must be included or explicitly rebuildable.
6. Document protocol/schema capabilities, upgrade requirements, limitations and measured results. Update changelog and pin matching engine/plugin artifacts; tag a working checkpoint before starting the next milestone.

### Migration policy

Additive optional fields first. Separate data schema version from engine/plugin release numbers. Dry-run migrations, checkpoint progress and preserve original IDs. Do not fabricate provenance or historical validity for old records. A search index can be rebuilt; accepted revision history and operation receipts cannot.

Old-client writes must receive safe defaults. Old-server/new-client combinations must negotiate capabilities or reject unsupported operations explicitly; never silently discard evidence or correction fields. Binary rollback after schema migration is not guaranteed: recover through a verified compatible backup or an explicitly tested downgrade.

### Production gate

Follow `docs/BACKUPS.md`, including a restore drill. Any new SQLite store belongs on the persistent mount and needs a consistent backup with Qdrant; coordinate writers/checkpoints rather than copying a live database file blindly. Keep the embedding model/dimensions unchanged. Run authenticated read/write/search and cross-client smoke tests after upgrade. Verify filters, correction history, evidence resolution and recovery state—not just `/health`.

## Dependency order and first implementation slice

`M0 → M1 → M2 → M3 → M4 → M5 → M6`

Reliability tests and retrieval experiments can be developed earlier in isolation, but avoid concurrent edits to the plugin monolith and storage structs. Verification transition tests can proceed after M1 while temporal API design is reviewed. Do not defer the destructive-consolidation fix behind the full journal architecture.

**First slice:** write consolidation safety regressions, stop legacy source deletion, fix query-aware prefetch with session/result isolation tests, and record baseline test output. Ship that checkpoint before evidence/schema expansion. Diagnose live auth only within authorized configuration scope.

Suggested separate commits per checkpoint: regression tests; engine/storage change; API/client integration; UI; migration/backup tests; documentation/release metadata. Keep unrelated Compose changes out of those commits.

## Design references

These are idea sources, not vetted dependencies or reproduced benchmarks:

- Neural Mesh — supersession, temporal retrieval, hybrid search: https://github.com/D0xedDevi0/hermes-neural-mesh
- Aletheia — evidence-bound scoped recall and receipts (licensing review required before code reuse): https://github.com/ousiaresearch/aletheia
- Dreaming — verify derived notes against source transcripts: https://github.com/hsyvy/dreaming-memory
- Oneiros — proposals, apply receipts, conflict-aware rollback: https://github.com/noblebrown-69/hermes-oneiros
- Assumption Ledger — uncertainty is not contradiction: https://github.com/itzebe/hermes-assumption-ledger
- Renor — deadline-bounded recall and bounded writes: https://github.com/renorai/hermes-renor
- Backfill — dry-run, resumable provider replay and recall checks: https://github.com/wth-amna/hermes-backfill
- MemDiffs — visible persistent-context changes: https://github.com/daji-dev/mem-diffs
- CMI — separate context-management experiment: https://github.com/jeremiebergel/context-manager-intelligence
