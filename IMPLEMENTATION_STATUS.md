# memex8 Implementation Status

## 2026-11-08 — Compact retrieval and batch get (roadmap E)

- Added opt-in `compact=false` search/recall parameters to REST, both Rust MCP
  transports, and Hermes. A shared projection emits seven metadata fields with
  Unicode-safe snippets capped at 160 characters, preferring nonblank summaries.
  Full response shapes, ranking, pagination, and access tracking are preserved.
- Added GET query search alongside existing POST JSON search and authenticated
  ordered REST batch get. MCP/Hermes `memex8_get` accepts one of `memory_id`,
  legacy `id`, or `ids` (1–50), rejects ambiguous arguments, preserves repeated
  found IDs and lists unavailable IDs in `not_found`. Batch reads full payloads
  with one Qdrant request. No new config keys or payload migrations.
- Tool descriptions teach compact search followed by selective full retrieval.
  Hermes auto-prefetch and `_build_context_block` remain unchanged.
- Validation: formatting/build and Rust suites passed (74 lib, 74 binary, 38 core
  integration; existing kmeans skip). Disposable-Qdrant tests passed for REST,
  stdio MCP, Qdrant/tag-filter/TurboVec paths, Unicode snippets, full payloads,
  batch ordering/repetitions/missing IDs/limits, auth, and default compatibility.
  Long-body fixture: full 12,230 vs compact 1,406 serialized UTF-8 JSON bytes (11.5%).
  Hermes transport/schema/dispatch tests passed. No deployment or live-data changes.

## 2026-11-08 — Safe duplicate removal (roadmap D)

- Full slumber supports archived removal when `dedup_dry_run=false`; the shipped
  default remains true and `slumber inspect-duplicates` is always read-only.
  Candidate eligibility adds project-tag scope and restored-point cooldowns.
- Added backward-compatible dedup journal kinds to the existing archive. Full loser
  snapshots/vectors and pre-merge winner payloads are verified before mutation.
  Winner tags/ancestry/evidence and an idempotency marker are written together;
  per-identity access contributions avoid repeated counting and shared-ancestry
  inflation. Existing derivation/certification metadata and content are retained.
- Prepared operations resume after interruption and recheck current vectors,
  source snapshots, ordering, scope, and protection. Changed operations are
  cancelled with archives retained. Reports distinguish candidates, actual removals,
  skipped pairs, and journal IDs. Failures reset slumber status to idle.
- Added REST/MCP/Hermes duplicate restoration with current-model re-embedding,
  preservation of existing source edits/certification, retry state, and 30-day
  dedup/consolidation cooldowns. Winner metadata is retained during restoration.
  Existing CLI backups carry both journal kinds; old consolidation journals default
  to their original kind. Consolidation undo accepts appended dedup ancestry and
  gives its restored sources both cooldowns to prevent immediate removal.
- Invalidated in-memory/disk search caches before removal/restore; cleaned dangling
  graph/association references. Added archive kind/state and association keyword
  indexes, including existing collections, for bounded retrieval of affected records.
- Validation: formatting/build passed; Rust suites passed (71 lib, 71 binary,
  38 integration; existing kmeans skip). Seven disposable-Qdrant dedup scenarios,
  five consolidation recovery scenarios, certification/inspection, Hermes contracts,
  and backup/path/bridge regressions passed. Existing configuration keys only; no
  deployment or live-data changes. Independent writer processes require serialization.

## 2026-11-08 — Provenance and recoverable consolidation (roadmap C)

- Added default-safe derivation timestamps, consolidation cooldowns, and evidence
  fingerprints alongside direct source IDs. Full GET/import/export retain them.
  Typed consolidation options carry provenance without arbitrary payload overrides.
- Added durable `consolidation_archive` journals with complete typed source payloads,
  original vectors, capture-time embedding configuration, and operation state.
  Archives and replacement summaries are read back before source deletion; deletion
  rechecks content/type/visibility and certification in Qdrant.
- REST, both Rust MCP transports, and Hermes expose unconsolidate. Missing sources
  are re-embedded with the current model; existing source edits/certification survive.
  All restored/retained sources receive at least a 30-day consolidation cooldown.
  Sources and cooldowns are read back before deleting the summary. Retrying partial
  work is safe; completed retries do not resurrect later deliberate deletions.
- Undo rejects edited/certified summaries, handles preserved/legacy sources, and
  invalidates local TurboVec memory/disk caches. Existing memory GET now returns
  404 for missing IDs. Maintenance, edit/import, payload changes, explicit deletion,
  certification, backup, and undo coordinate through an exclusive local gate.
- CLI backups include journals; restore accepts older backups without journals and
  refuses to downgrade existing journal state. Archives are retained without cleanup.
  Independent writer processes require external serialization.
- Added failure/retry/cooldown/search/backup recovery integration coverage against
  disposable Qdrant and local mock models, plus Rust and Hermes contract tests.
- Validation passed: `cargo build`, `cargo fmt --check`, and Rust tests (68 per
  unit-test target plus 38 core integration tests, with the existing k-means skip).
  Five disposable-Qdrant recovery scenarios, certification and read-only duplicate
  suites, Hermes consolidation/certification/visibility contracts, existing backup,
  export/import path, and shared-memory bridge tests passed. CI includes the new
  suites and runs for dependent PRs. Existing compiler warnings remain.
  No new config keys, deployment, or live-data changes.

## 2026-11-08 — Certified memories (roadmap B)

- Added default-safe certification timestamp, attribution, and note fields; the
  verification summary now counts certified memories separately. Existing Qdrant
  collections receive the verification-status keyword index on startup.
- REST certify/revoke endpoints, both Rust MCP transports, and Hermes support
  `memex8_certify`. Remember accepts `certified=false` by default and stores explicit
  certification in the initial point write. Rust also exposes `memex8_remember` as
  an alias of its existing `memex8_store` tool.
- Certified memories skip consolidation, prune/auto-archive, importance decay,
  session reweighting, and verification; recall/search apply no Weibull decay and a
  1.05 verification multiplier. Realm movements retain their payloads. Qdrant-side
  filters protect automatic updates/deletes against certification changes from
  another client. Local maintenance and certification use a shared coordination lock.
- Edit/import preserve complete metadata and require revocation before replacing
  an existing certified point. Certification flags reject string/number substitutes.
  Optional `by` is a caller-provided label, not authenticated identity.
- Added disposable-Qdrant lifecycle tests (two full slumber runs, revocation,
  concurrent verification protection, REST/MCP contracts, and import preservation),
  Hermes contract tests, and CI execution for both disposable-Qdrant suites.
- Validation: build, formatting, and Rust tests passed (66 tests in each unit-test
  target plus 38 core integration tests, with the existing k-means skip). Both
  disposable-Qdrant suites and Hermes certification/visibility contracts passed.
  Existing export/import, shared-memory bridge, and backup tests also passed.
  No new configuration keys, live-data maintenance, or deployment.

## 2026-11-08 — Duplicate detection (roadmap A)

- Added read-only `memex8 slumber inspect-duplicates` with original-vector cosine
  checks, deterministic winners, certified/type/realm/visibility protection, and
  bounded pair comparisons. Phase 1 now reports candidates separately from actual
  deletions; removal remains disabled pending durable archives and recovery.
- Equal file hashes require equal nonempty content for exact-match candidates.
- Consolidation records direct source IDs, excludes existing summaries and certified
  input, and skips already summarized source sets. Replacement storage precedes
  deletion, and only selected sources can be removed. Full archival recovery and
  certification lifecycle protection remain packages B–D.
- Config: `slumber.dedup_threshold` (0.95), `dedup_dry_run` (true), and
  `dedup_max_comparisons_per_realm` (1000000).
- Validation: `cargo build`, `cargo fmt --check`, and
  `cargo test -- --skip test_kmeans_simple` passed (62 unit tests in each Rust
  target, plus 38 core integration tests). The explicitly skipped k-means test
  remains outside this change. Existing unused-import/mutability warnings remain.
  `python3 tests/test_dedup_inspection.py` passed against disposable Qdrant,
  confirming the four-point fixture yields two candidates with unchanged vectors,
  payloads, and collection list. No live-data maintenance or deployment performed.

> Last updated: 2026-04-13

## Build Status
- ✅ Compiles clean: `cargo build --release`
- ✅ Binary: 8.4MB (stripped, LTO)
- ✅ 42 Rust files, ~4000 LOC
- ✅ 22 CLI commands, all functional

## Component Status

| Component | Status | Details |
|-----------|--------|---------|
| **Config** | ✅ Complete | TOML config with all sections, env overrides, defaults |
| **CLI** | ✅ Complete | 22 commands via clap derive, all wired to engine |
| **Storage (Qdrant)** | ✅ Complete | Real qdrant-client 1.17.0 integration, 3 collections, payload indexes, full CRUD, search, scroll, realm management |
| **Embedders** | ✅ Complete | Ollama (`/api/embed`) + OpenAI, batch support, trait-based |
| **Chunker** | ✅ Complete | Section (H2), paragraph, file strategies, token limits, heading tracking |
| **Ingester** | ✅ Complete | File + directory ingestion, SHA-256 dedup, walkdir with ignores |
| **Engine** | ✅ Complete | All 20+ methods wired: search, store, ingest, recall, realms CRUD, merge, upvote, prune, archive, edit, import, export, graph_search, slumber |
| **REST API** | ✅ Complete | Axum 0.8, 16 routes, CORS, tracing, state management |
| **MCP Server** | ✅ Complete | JSON-RPC 2.0 over stdio, 11 tools, graceful Qdrant fallback |
| **Doctor** | ✅ Complete | Qdrant connectivity, Ollama/OpenAI checks, config validation |
| **Quantizer** | ✅ Working | Adaptive scalar quantization with per-vector range, 7.6x compression @ 3.5-bit |
| **Slumber Engine** | ⚠️ Stub | Pipeline structure defined, actual re-clustering/quantization not implemented |
| **Knowledge Graph** | ⚠️ Stub | Entity extraction and relationship tracking |
| **MEMEX8.md Writer** | ⚠️ Stub | Write-back to project directories |
| **Web UI** | ⚠️ Empty | React + Three.js planned |
| **File Watcher** | ⚠️ Stub | `notify` crate integration planned |
| **Integrations** | ✅ Complete | Config generators for OpenClaw (webhooks), Hermes (MCP), pi.dev (extension) |

## Integration Paths

### Hermes Agent (MCP)
```bash
memex8 integration hermes  # outputs MCP server config
# Add to ~/.hermes/config.yaml:
#   mcp_servers:
#     memex8:
#       transport: stdio
#       command: memex8
#       args: ["mcp"]
```

### OpenClaw (Webhooks)
```bash
memex8 integration openclaw  # outputs webhook hook config
```

### pi.dev (Extension)
```bash
memex8 integration pi > ~/.pi/agent/extensions/memex8.ts
```

## Key Decisions Made

1. **Qdrant 1.17.0 client** — Using builder API (`CreateCollectionBuilder::new()`, etc.)
2. **Ollama `/api/embed` endpoint** — Not the deprecated `/api/embeddings`
3. **Tracing to stderr** — Required for MCP where stdout is JSON-RPC
4. **Realm centroids stored as payload arrays** — Qdrant stores centroid vectors in realm payload
5. **Memory importance = upvotes × recency × access_count** — Composite scoring for recall
6. **MCP graceful degradation** — Server responds to initialize/tools/list even without Qdrant

## Next Steps (Priority Order)

1. **Implement ScalarQuant codebook** — adaptive per-vector range, uniform scalar quantization, bit-packing
2. **Implement slumber pipeline** — Quantize, re-cluster (k-means), merge small realms, prune
3. **Add file watcher** — `notify` crate for real-time directory watching
4. **MEMEX8.md write-back** — Generate memory files for model context pickup
5. **Knowledge graph** — Entity extraction from chunks, relationship tracking
6. **Web UI** — React SPA with card view + 3D knowledge graph
