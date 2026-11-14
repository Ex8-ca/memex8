# memex8 improvement execution plan

Date: 2026-11-08  
Status: A–H merged and deployed to 192.168.1.3 (2026-11-09 UTC)
Source: `PLAN-2026-11-08-trending-improvements.md` (original brief, preserved)

Follow-up: [gbrain-inspired implementation plan](PLAN-2026-11-08-gbrain-improvements.md).
The baseline notes below describe the start of the original work; they are historical.

## Outcomes

1. Stop repeated summaries from crowding out useful search results.
2. Protect human-confirmed memories and make automatic merges recoverable.
3. Reduce retrieval payloads and make weak search matches explicit.
4. Support chronological exploration and redact private input before processing.

This document organizes the seven original items into independently reviewable PRs.
It does not authorize deployment, production maintenance, or deletion of live data.

## Current baseline and corrections to the brief

- Actual checkout: `/home/marc/nvme-data/Documents/myprojs/memex8`.
  The session workspace `/home/marc/myprojs/memex8` is now the isolated implementation worktree.
- Branch: `fix/slumber-respect-config-thresholds`, with existing tracked changes in
  seven files, plus untracked plans, configuration backups, and other files.
  Preserve everything; do not commit unrelated files or secret-bearing backups.
- Confirmed in current code: `deduplicate()` uses only `source_hash`, ignores its
  declared 0.95 threshold, and deletes immediately. It currently prefers importance.
- `SlumberReport.deduplicated` exists and currently counts actual deletions.
  Preserve that meaning; add a separate dry-run candidate count.
- The current REST search handler reads a JSON `SearchRequest`; the Hermes client
  also sends JSON. Verify route methods before implementation, preserve the existing
  contract, and do not assume the brief's proposed GET endpoint already exists.
- `Cargo.toml` declares `qdrant-client = "1.12"`; check the resolved lockfile and
  deployed Qdrant version before choosing datetime filtering/ordering APIs.
- `IMPLEMENTATION_STATUS.md` contains old component descriptions. Add dated entries
  for this work rather than treating that document as a reliable current inventory.

## Working rules

- One focused branch and PR per work package below; dependent PRs land in order.
- First review and checkpoint the existing changes with their current work. Do not
  automatically stash or commit another task's edits just to begin this roadmap.
- Add new payload fields with backward-compatible defaults; update both explicit
  payload serializers/deserializers and ordinary serde structures.
- Keep REST, Rust MCP schemas/dispatch/output, and Hermes schemas/client/output in
  sync whenever a capability affects them. Preserve existing default responses.
- Respect existing visibility, authorization, realm, and tag restrictions on every
  new retrieval path, including batch get, timeline, and suggested realms.
- Update `config.example.toml` for new keys and add a dated implementation-status
  entry in each implementation PR.
- Never wipe Qdrant collections. Use isolated fixtures for destructive tests.
- A dedup dry run only protects the dedup phase. A complete slumber run can still
  prune, consolidate, decay, and rewrite data: use an isolated snapshot or a truly
  detection-only entry point for initial validation.

## Sequence

### A. Duplicate detection and immediate recurrence guard — original Item 0, part 1

Branch: `fix/slumber-near-dup-detection`  
Dependencies: existing branch work reconciled

- [ ] Audit `source_hash` semantics before retaining exact-hash grouping: if it is
  a file-level hash shared by distinct chunks, equal hashes alone must not justify
  deleting those chunks.
- [ ] Separate candidate discovery from mutation for both exact and near matches.
- [ ] Add `[slumber] dedup_threshold = 0.95` and `dedup_dry_run = true`, validating
  the threshold. Keep actual near-duplicate deletion disabled until package D.
- [ ] Use current original vectors within a realm for cosine checks. Association
  links can shortlist candidates, but stale links or compressed scores must not
  authorize deletion. Bound work per realm and measure cost on a representative copy.
- [ ] Protect differing long-lived memory types. Do not cross visibility boundaries.
  Never eliminate a memory solely because it is transitively linked to a duplicate:
  each proposed loser must meet the threshold against its retained winner.
- [ ] Make winner selection deterministic: certification, upvotes, importance,
  newest ingestion time, then ID. Preserve every certified point in a conflict.
- [ ] Add report fields for candidates, actual removals, protected/skipped points,
  and dry-run status. Log IDs, scores, and reasons without dumping content.
- [ ] Add a conservative consolidation guard for known derived summaries. Full
  ancestry-aware recurrence prevention follows in package C; old summaries without
  provenance cannot be identified with certainty.

Done when: three mutually similar vectors (cosine >= 0.97) plus one distinct vector
produce two proposed losers and no writes in dry run; tests cover chain similarity,
distinct chunks with a shared source hash, type boundaries, and deterministic ties.
Validate a detection-only run against an isolated copy of representative data.

### B. Certified memories — original Item 6

Branch: `feat/certified-memories`  
Dependencies: A

- [x] Add `certified` status, `certified_at`, and `certified_by`, with default-safe
  payload handling and a keyword index where needed. Decide and document storage
  of the optional certification note; do not silently discard it.
- [x] Add REST certify/revoke endpoints and `memex8_certify` to both tool surfaces.
- [x] Add `certified: false` by default to remember/store. Certification requires
  explicit user intent; ordinary memory capture must not imply human confirmation.
- [x] Centralize protection checks and audit dedup, consolidation, pruning,
  auto-archive, decay, verification, realm split/merge, and indirect deletion paths.
  Realm movement is allowed; automatic content rewriting/deletion is excluded.
- [x] Give certified search results the specified 1.05 verification multiplier.
- [x] Define revocation as clearing certification metadata and returning the point
  to `unverified`, retaining any separate audit history.

Done when: after certification and two isolated slumber cycles, the point's content
and protected fields remain unchanged and it remains present. Revocation restores
normal lifecycle behavior. Two certified duplicates both survive automatic dedup.

### C. Provenance and recoverable consolidation — original Item 5

Branch: `feat/consolidation-provenance`  
Dependencies: B

- [x] Add `consolidated_from`, `derived_by`, `consolidated_at`, and
  `no_consolidate_until` with defaults. Expose provenance in full retrieval.
- [x] Prefer a typed write-options structure over more positional arguments or
  unrestricted extra payload that could override protected fields.
- [x] Record direct source IDs and resolve ancestry for recurrence checks. Skip
  single-summary groups and groups that add no new underlying evidence; partial
  ancestry overlap alone must not suppress genuinely new information.
- [x] Use a dedicated durable Qdrant archive collection for removed originals.
  Archive complete payloads and vectors, not only four fields: restore must retain
  tags, visibility, realm, certification, dates, scores, and identity. Record the
  embedding model/dimension so vectors can be reused safely or re-embedded.
- [x] Verify archive writes and summary persistence before deleting any source.
  Track operation state so partial failures and retries are recoverable.
- [x] Implement REST unconsolidate with authorization and conflict handling.
  Restore all sources durably before deleting the summary; retries must be safe.
- [x] Apply a 30-day consolidation cooldown to restored points and retain it across
  updates. With preserved originals, use existing source points without overwriting
  newer edits. If an old summary has no recoverable sources, return a clear error.

Done when: consolidation exposes source IDs; unconsolidation restores original IDs
and metadata; the next slumber honors cooldown. Injected failures during archive,
source deletion, and restore do not cause unrecoverable loss or duplicate restores.

### D. Safe duplicate removal — original Item 0, part 2

Branch: `fix/slumber-near-dup-removal`  
Dependencies: A, B, C

- [x] Archive losers before removal, merge tags and access counts into the winner,
  and record loser IDs and dedup provenance without losing existing ancestry.
- [x] Make the metadata merge idempotent: retries must not add access counts twice.
  Recheck protection and relevant state immediately before mutation.
- [x] Keep `dedup_dry_run = true` as the shipped default. Disabling it is an explicit
  operator action after candidate review and a verified recovery procedure.
- [x] Refresh/invalidate TurboVec and other affected cached references through the
  existing consistency mechanism so removed IDs stop surfacing in retrieval.

Done when: the four-vector fixture retains one winner plus the distinct point,
metadata is merged once, archives are recoverable, repeated runs are stable, and
protected points survive. Production deletion remains a separate rollout step.

### E. Compact retrieval and batch get — original Item 1

Branch: `feat/compact-search-batch-get`  
Dependencies: baseline only; may be scheduled after A if retrieval is more urgent

- [x] Add `compact = false` to search and recall on all supported interfaces.
- [x] Share one compact representation:
  `{id, snippet, realm_name, score, ingested_at, memory_type, verification_status}`.
- [x] Use summary when available, otherwise content, with a consistent maximum of
  about 160 Unicode characters for either source. Cover emoji and multibyte text.
- [x] Add ordered REST batch get and `memex8_get(ids=[...])`, maximum 50 IDs, while
  preserving `memory_id`. Reject simultaneous single/batch arguments. Preserve
  requested order including repeated found IDs; list unavailable IDs in `not_found`
  without disclosing inaccessible records.
- [x] Document the compact-search-then-fetch pattern in both tool descriptions.
  Leave Hermes `_build_context_block` and automatic prefetch unchanged.

Done when: existing clients retain their default response; batch get returns three
valid IDs in requested order and lists one missing ID. A recorded representative
long-body fixture yields compact serialized responses <= 25% of full response bytes
at equal result count. Treat that as a measured target, not a guarantee for short
memories; report bytes separately from token estimates.

### F. Search coverage — original Item 4

Branch: `feat/search-coverage`  
Dependencies: E

- [x] Always return coverage for search, including empty results and offset pages.
  Add `[search.coverage] high = 0.55` and `partial = 0.40`, validating their order.
- [x] Define `best_score` as the best eligible raw cosine candidate before minimum
  score filtering and pagination, distinct from boosted ranking scores. Use null
  when there are no eligible candidates; define that case as `level = none`.
- [x] Define `results_above_min` as eligible candidates meeting `min_score` in the
  bounded candidate set, before pagination; document it as a candidate count rather
  than a collection-wide total. Compute identical semantics on TurboVec and Qdrant.
- [x] Rank up to three accessible realm centroids using the same query embedding.
  Handle absent/stale centroids gracefully and respect request filters.
- [x] Put coverage first in Rust MCP and Hermes text output. Keep hints useful
  without making an installation-specific digest path a universal assumption.
- [x] Label this a retrieval-strength heuristic, not proof that a query is answerable.
  Calibrate defaults using known-hit and known-miss fixtures before rollout.

Done when: fixed known-hit/miss fixtures produce expected levels on both retrieval
paths, and a query with no results still has coverage and available realm hints.

### G. Timeline — original Item 2

Branch: `feat/memory-timeline`  
Dependencies: E

- [x] Accept exactly one anchor: memory ID, query, or RFC3339 timestamp; default
  before/after to 5 and cap each at 50. Resolve query anchors within caller scope.
- [x] Return shared compact rows through engine, REST, MCP, and Hermes.
- [x] Normalize timestamps to UTC and add a Qdrant datetime index if supported.
  Do not substitute a keyword index for datetime range/ordering semantics.
- [x] Prefer indexed nearest-before/after queries; otherwise paginate an expanding
  time window with explicit work limits. Never sort only the first scroll page and
  assume it contains the nearest neighbors. Report truncation if a work cap is hit.
- [x] Include the anchor for ID/query mode; timestamp mode has no synthetic row.
  Use `(ingested_at, id)` for deterministic order when timestamps tie. Return fewer
  neighbors at dataset boundaries; use an explicit no-match result for query misses.

Done when: a known anchor has N available older and newer neighbors in ascending
order, with tests for equal timestamps, realm restrictions, boundaries, missing
anchors, and neighbors beyond a single Qdrant scroll page.

### H. Private-input redaction — original Item 3

Branch: `feat/private-redaction`  
Dependencies: baseline only; independent and suitable to bring forward after A

- [x] Build one reusable sanitizer and make every applicable ingestion path invoke
  it before embedding, LLM processing, chunking, content hashing, or persistence.
  Audit direct store, file ingest, session ingest, edits, imports, and derived writes
  so the guarantee does not depend on one API entry point.
- [x] Replace closed private spans with `[private]`, case-insensitively across
  multiple lines. Drop from an unclosed opening tag to end; handle nested tags
  conservatively with a parser/state machine rather than relying only on regex.
- [x] Track whether meaningful non-private content remains. Marker-only output
  counts as empty: direct REST storage returns 422 with the specified error; batch
  and file/session ingest skip/reject empty units with an explicit safe diagnostic.
- [x] Inspect logging and error paths for raw content leakage. Test embedder/LLM
  inputs as well as stored payloads; a payload-only check is insufficient.
- [x] Document that redaction affects new writes and cannot erase existing stored
  private text. Defer configurable secret-pattern regexes to a separate proposal.

Done when: single/multiple/multiline/nested/unclosed/all-private cases pass; spies
observe no private text reaching embedding or LLM calls; isolated Qdrant payloads
and captured logs contain no private text. Include a tag spanning a chunk boundary.

## Validation and rollout

For every implementation PR:

- [ ] Run `cargo fmt --check`, `cargo build`, and
  `cargo test -- --skip test_kmeans_simple`; format before the check as needed.
  Linking requires `libopenblas-dev`. Record the skipped test explicitly.
- [ ] Run relevant existing Hermes tests for tool/client changes, and targeted
  integration checks against an isolated Qdrant collection/instance where needed.
- [ ] Test old payloads, omitted new parameters, and parity across affected tool
  schemas, dispatch, and output formatting.
- [ ] Record actual commands/results and update config examples and status notes.

No container rebuild, restart, or live slumber trigger is part of planning or PR
completion. When deployment is requested: verify backups and restore on a copy,
deploy with dedup dry run enabled, inspect candidates and search measurements, then
make deletion enablement a separate explicit decision. Disabling a feature does not
undo deleted/merged data; recovery uses the archives.

## Execution log

### 2026-11-08 — Package A

Merged: https://github.com/Ex8-ca/memex8/pull/25 (`6c32f3b`).

- Created clean worktree `/home/marc/myprojs/memex8` on
  `fix/slumber-near-dup-detection`, based on committed `21304c7` from the existing
  threshold branch. Original uncommitted changes remain in their checkout.
- Implemented read-only duplicate inspection, bounded comparisons, reporting, and
  content-aware consolidation recurrence prevention.
- Added `consolidated_from` earlier than package C because recurrence detection
  needs it. Full provenance timestamps, archives, and undo remain package C.
- Corrected consolidation replacement ordering and limited deletion to prompt
  sources; excluded certified points and existing summaries from its inputs.
- Added Rust regression tests and a disposable-Qdrant CLI integration test.
- Validation passed: build, formatting, Rust tests (62 per unit-test target plus 38
  integration tests, with the specified k-means skip), and read-only CLI integration
  against an ephemeral Qdrant instance. Existing compiler warnings remain.
- Representative production-scale measurements and live candidate review remain
  rollout work; no live slumber or deployment was performed.

### 2026-11-08 — Package B

Draft PR: https://github.com/Ex8-ca/memex8/pull/26 (all three CI checks passed).

- Implemented certification from merged main on `feat/certified-memories`.
- Added certify/revoke to REST, both Rust MCP dispatchers, and Hermes; ordinary
  capture stays uncertified and explicit remember certification is atomic.
- Audited lifecycle phases and guarded background payload updates/deletes at Qdrant.
  Added local coordination between certification, maintenance, and content replacement.
- Added full snapshot import preservation and refused replacement of certified IDs
  until revocation. Added the verification-status index to existing collections.
- Added disposable-Qdrant lifecycle/concurrency/MCP/import checks and Hermes tests;
  integrated the suites into CI. No live service or data was changed.
- Validation passed: build, formatting, 66 tests in each Rust unit-test target,
  38 core integration tests, both disposable-Qdrant suites, and Hermes contracts.
  The existing k-means skip and compiler warnings remain.

### 2026-11-08 — Package C

Draft PR: https://github.com/Ex8-ca/memex8/pull/27 (all three CI checks passed).

- Built on `feat/certified-memories` in a separate dependent branch,
  `feat/consolidation-provenance`; B remains a separate PR.
- Added durable full-source/vector journals, provenance and underlying evidence
  fingerprints, typed summary metadata, checked archive/summary persistence, and
  conditional source deletion. Near-duplicate removal remains package D.
- Added REST/MCP/Hermes undo, source/cooldown read-back, preserved-source edit
  handling, legacy recovery errors, retry states, and local index invalidation.
- Serialized local maintenance/content mutations/recovery and added journals to
  existing CLI backup/restore. Docker volume backups already include all collections.
- Integration tests cover archive write failure, interrupted deletion, partial
  restore failure/retry, metadata preservation, cooldown, immediate search visibility,
  and backup restoration to an isolated instance. All five scenarios passed.
- Build, formatting, 68 tests in each Rust unit-test target, 38 core integration
  tests, and the existing certification/duplicate-inspection/plugin/backup/bridge
  regressions passed. The existing k-means skip and compiler warnings remain.
- No deployment or live-data changes. Archives are retained; independent writer
  processes need external serialization.

### 2026-11-08 — Package D

- Built on the recovery branch in `fix/slumber-near-dup-removal`; C remains a
  separate dependent PR. Default dry run remains enabled; no deployment performed.
- Added archive-before-delete operations, atomic winner metadata/marker writes,
  per-identity access accounting, protection/scope/vector/snapshot rechecks,
  interrupted-operation handling, and explicit duplicate recovery across all tools.
- Added 30-day restored-point cooldowns, indexed orphan-reference cleanup, cache
  invalidation, and candidate/removal/skipped/operation-ID reporting. Existing
  backup/restore includes both journal kinds and accepts older consolidation journals.
  Consolidation undo remains available after dedup and applies both cooldowns.
- Added Rust accounting/scope tests and disposable-Qdrant removal/failure/retry/
  recovery/certification/backup tests, plus Hermes contracts. Formatting/build,
  71+71+38 Rust tests (existing kmeans skip), all seven dedup and five consolidation
  recovery scenarios, and certification/inspection/plugin/backup/path/bridge
  regressions passed.
- Future live rollout still requires candidate review and explicit removal enablement.

### 2026-11-08 — Package E

- Merged B, C and D in dependency order after required checks passed. Created
  `feat/compact-search-batch-get` from updated main.
- Added compact REST/MCP/Hermes search and recall, shared Unicode-safe snippets,
  ordered full batch get (1–50 IDs), legacy single-ID aliases and validation.
  Existing responses/prefetch remain compatible; no config keys or migrations.
- Rust/format/build and disposable-Qdrant REST/stdio/index-path tests passed;
  Hermes contracts passed. Representative UTF-8 JSON bytes measured 12,230 full
  versus 1,406 compact (11.5%). No deployment or live-data changes.
- Next package: F, query coverage and realm suggestions.

### 2026-11-08 — Package F

- Merged #29 after all three required checks passed, then created
  `feat/search-coverage` from updated main.
- Added shared raw-cosine coverage, configurable/validated thresholds, filtered
  live-member realm hints, and coverage-first Rust MCP/Hermes search text.
  Kept ranking scores separate; a payload-free Qdrant probe (max 100) gives the
  same coverage semantics on Qdrant and TurboVec, including empty offset pages.
- Known-hit/partial/miss fixtures, request filters, bounded candidate count,
  missing/orphan centroids, query-embedding reuse and tool contracts passed.
  Formatting/build and 77+77+38 Rust tests (existing kmeans skip), compact retrieval,
  certification/consolidation/dedup recovery, Hermes, export/import path and bridge
  regressions passed; no deployment or live-data changes.
- Next package: G, timeline retrieval around a memory/query/time anchor.

### 2026-11-08 — Package H (before G)

- Brought private redaction forward after #30 merged, per the agreed priority.
  Created `feat/private-redaction` from main.
- Added one parser across direct/file/watched/session/webhook/edit/import and
  derived capture, including embedding-boundary enforcement and storage guards.
  Redaction precedes chunking/source hashing and session truncation/LLM calls.
- Marker-only inputs reject/skip safely. Imports re-embed changed bodies; exact
  recovery rejects marked legacy snapshots. Existing stored data is not rewritten.
- Five isolated privacy scenarios passed with provider/payload/log spies, plus
  existing recovery/retrieval/certification/Hermes/path/bridge regressions.
  Formatting/build and 80+80+38 Rust tests passed (existing kmeans skip/warnings).
  No deployment or live-data changes.
- Next package: G, timeline retrieval.

### 2026-11-08 — Package G

- Merged #31 after all required checks passed; created `feat/memory-timeline`
  from updated main for the final original roadmap item.
- Added scoped ID/query/time anchors, shared compact output across REST/Rust MCP/
  Hermes, UTC output, and a genuine datetime index for existing collections.
- Ordered range queries fully paginate boundary ties, sort by timestamp/ID,
  and expose truncation at a fixed 10,000-row budget. Query misses are explicit.
- Disposable-Qdrant tests cover multi-page ties, offset timestamps, scope,
  boundaries, zero counts, validation, missing/corrupt anchors, query hits/misses,
  stdio MCP parity, submicrosecond precision, and budget exhaustion. Formatting/
  build, 81+81+38 Rust tests (existing kmeans skip), three timeline scenarios,
  compact/coverage regressions and 24 Hermes tests passed. No deployment or
  live-data changes.
- Original packages A–H are implemented; live rollout and deferred options
  remain separate follow-up work.

## First work session

1. Review the existing threshold/config work and its uncommitted slumber changes;
   agree on the clean branch base without discarding current work.
2. Implement package A only: candidate discovery, reporting, and recurrence guard.
3. Review its tests and detection report, then proceed to B and C before D.

Default order: A → B → C → D → E → F → G → H. E and H are independent options to
bring forward after A without enabling destructive duplicate removal early.

Deferred: chill capture mode, entity synonyms, dedicated reranking, configurable
secret-pattern redaction, and a general backup/restore product feature. The archive
and recovery work needed for these automatic mutations remains in scope.
