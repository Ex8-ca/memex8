# memex8 Improvement Brief — 2026-11-08

Source: GitHub weekly trending review (claude-mem, NVIDIA/auto-ontology).
Everything you need is in this file. You do not need to visit GitHub.

## Context for the implementer

- **Repo:** `~/nvme-data/Documents/myprojs/memex8` (Rust, ~16.5k LOC). Origin = github `Ex8-ca/memex8`.
- **Stack:** axum REST API (`src/api/`), Qdrant storage (`src/storage/qdrant.rs`), engine (`src/engine/`), MCP server (`src/mcp/tools.rs`, `src/mcp/server.rs`), Hermes Python plugin (`plugins/memex8/__init__.py`). TurboVec compressed index is used for search when there's no tag filter (`Engine::search`, `src/engine/mod.rs:619`).
- **Two tool surfaces — keep them in sync:** the Rust MCP tool list (`src/mcp/tools.rs`) AND the Hermes plugin tool schemas (`plugins/memex8/__init__.py`, around lines 390–505). A new tool or new parameter goes in both.
- **Core types:**
  - `MemoryPoint` (`src/storage/qdrant.rs:15`): the full Qdrant payload. Fields include `id, content, summary, source_file, realm_id, realm_name, importance, upvotes, tags, ingested_at, last_accessed, access_count, chunk_type, heading, source_hash, related_memory_ids, association_strengths, reaction_score, topic_clusters, quantized_bit_width, last_verified, verification_confidence, verification_status, memory_type, visibility`.
  - `MemoryResult` (`src/engine/mod.rs:28`): what search/recall return. Currently `id, content, heading, realm_name, importance, score, last_accessed, access_count, upvotes, related_memory_ids, association_strengths`.
  - `StoreRequest` (`src/api/routes/memories.rs:9`): `content, tags, realm_hint, source, visibility, memory_type`.
- **Slumber pipeline** (`src/engine/slumber.rs`, 3000 LOC): Phase 1 `deduplicate()` (L221), 3b `split_large_realms` (L387) / `merge_similar_realms` (L639), prune (L1538), Phase 6 `llm_consolidate()` (L1831), decay (L2200), `prune_empty_realms` (L2523), Phase 13 `verification_sweep()` (L2548).
- **New payload fields:** add them with `#[serde(default)]` so old points still deserialize. If you filter on a field, add a Qdrant keyword index next to the existing ones (`src/storage/qdrant.rs` ~L451).
- **Build/CI:** `cargo build` must have 0 errors; `cargo fmt` before committing; `cargo test -- --skip test_kmeans_simple`. Linking needs system `libopenblas-dev`.
- **Deploy (only if asked):** `docker compose build memex8 && docker compose restart memex8`; container `memex8-memex8-1`. Smoke-test slumber with `docker exec memex8-memex8-1 memex8 slumber trigger` (it prints each phase to stdout). For authed curl, run it inside the container using `$MEMEX8_API_KEY`.
- **Workflow:** one branch + PR per item, build-verify between items. The working tree currently has uncommitted changes on `fix/slumber-respect-config-thresholds` (`config.rs`, `engine/mod.rs`, `config.example.toml`, `TODO.md`). Commit or stash them before branching. Don't discard them.
- **Data safety:** never wipe Qdrant collections. Anything destructive (dedup deletes) needs a dry-run / log-only mode first.

---

## Item 0 — Fix near-duplicate dedup (BUG, do first)

**Problem:** `deduplicate()` (`slumber.rs:221`) declares `let _dedup_threshold = 0.95f32;` but never uses it. It only removes exact `source_hash` matches. Observed: one search returned 9 of 10 results that were LLM re-summaries of the same Oct-4 event, with pairwise association strengths of 0.98–0.997. Consolidation keeps re-summarizing a cluster, and each pass leaves another near-copy.

**Do:**
1. Keep the exact-hash pass.
2. Add a near-duplicate pass. Use `scroll_all_memories_with_vectors()` per realm, or reuse Phase 9 `related_memory_ids` / `association_strengths` (these already carry cosine scores). For pairs above `config.slumber.dedup_threshold` (new config key, default 0.95), keep the winner and delete the rest.
3. Winner rule: user-pinned/certified (Item 6) > higher upvotes > higher importance > newer `ingested_at`. Merge `tags` and `access_count` into the winner. Add the losers' ids to the winner's `consolidated_from` (Item 5) if that field exists.
4. Never dedupe across different `memory_type`s where one is long-lived (profile/preference/entity/relationship).
5. Add `dedup_dry_run` config (default true for the first release). It logs pairs it would delete. Report the count in `SlumberReport.deduplicated`.
6. Also fix the root cause in `llm_consolidate()`: skip a realm whose members are already a single consolidated summary, or whose members all have `consolidated_from` overlapping. That stops re-summarizing a summary.

**Accept:** a unit test with 3 vectors at cosine ≥0.97 plus 1 distinct vector → 1 kept + the distinct one. A dry-run slumber on live data logs the duplicate cluster with no deletions.

---

## Item 1 — Compact search + batch get (progressive disclosure)

**Idea (from claude-mem):** search returns a cheap index. The agent fetches full bodies only for the IDs it picks. claude-mem reports ~10x token savings (~50–100 tokens per index row vs ~500–1000 per full row).

**Do:**
1. Add `compact: bool` (default `false` for back-compat) to the REST search (`GET /api/v1/memories/search`), recall, the Rust MCP `memex8_search`/`memex8_recall`, and the Hermes plugin tools.
2. Compact row shape: `{id, snippet, realm_name, score, ingested_at, memory_type, verification_status}`. `snippet` = `summary` if set, else the first ~160 chars of `content`, cut on a UTF-8 char boundary (not a byte index; this repo has had UTF-8 slicing bugs before).
3. Make `memex8_get` accept `ids: [string]` (batch, max 50) as well as single `memory_id`. Add REST `POST /api/v1/memories/batch-get` with body `{ "ids": [...] }` that returns full `MemoryPoint`s in the requested order, with missing ids listed in `not_found`.
4. Update tool descriptions so the agent learns the pattern: "Use compact=true, then memex8_get(ids=[...]) for the ones that matter."
5. Hermes plugin's auto-injected context block (`_build_context_block`) is OUT of scope. Don't change prefetch behavior in this item.

**Accept:** compact search for a common query returns ≤25% of the bytes of full search. Batch get returns 3 ids in order and reports 1 bad id under `not_found`.

---

## Item 2 — `memex8_timeline` tool

**Idea (from claude-mem):** "what was happening around this memory" — chronological neighbors.

**Do:**
1. New engine fn `timeline(anchor: Anchor, before: usize, after: usize, realm: Option<&str>)`, where `Anchor = MemoryId(String) | Query(String) | Time(rfc3339)`. For `Query`, use the top search hit's `ingested_at` as the anchor time.
2. Implementation: a Qdrant scroll with an `ingested_at` range filter, ordered. If ordering by payload isn't available in our qdrant-client version, fetch a time window (±N days, widen until filled) and sort in Rust. Store `ingested_at` as RFC3339 consistently; add a datetime/keyword index if needed.
3. Return compact rows (same shape as Item 1).
4. Expose it as REST `GET /api/v1/memories/timeline?id=&query=&at=&before=5&after=5&realm=`, as MCP tool `memex8_timeline`, and as a Hermes plugin tool.

**Accept:** anchoring on a known memory returns it with N older and N newer rows in strict chronological order.

---

## Item 3 — `<private>` redaction

**Idea (from claude-mem):** anything wrapped in `<private>…</private>` never gets stored.

**Do:**
1. Strip `<private>...</private>` spans (case-insensitive, multiline, non-greedy, possibly several per input) in ONE choke point before embedding and before storage. That's `Engine::store_memory` (`src/engine/mod.rs:1188`) plus the ingest path (`Engine::ingest_path`, L279) plus the session/conversation ingest (`src/engine/session.rs`). Don't scatter the stripping across call sites.
2. Replace each span with `[private]` so sentence structure survives.
3. Unclosed `<private>` tag: drop everything from the tag to end of input (fail closed).
4. If the content is empty after redaction, return 422 "content empty after redaction". Don't store an empty memory.
5. Never log the raw pre-redaction content (check `tracing` calls along the path).
6. Optional extra: a configurable regex list `[redaction] patterns = [...]` for API-key shapes (`sk-...`, `glpat-...`, `nvapi-...`), off by default.

**Accept:** unit tests cover a single span, multiple spans, a multiline span, an unclosed tag, and an all-private input (→ 422). A Qdrant payload check after storing shows no private text.

---

## Item 4 — Answerability / coverage signal on search

**Idea (from auto-ontology `check_answerable`):** return a coverage score and say plainly when nothing covers the query, instead of empty or junk results. Today a miss is indistinguishable from "no data", and agents have to grep the digest `~/.memex8/memex8.md` by hand to tell them apart.

**Do:**
1. Add a `coverage` object to `SearchResponse` (`src/api/routes/memories.rs:99`), always present:
   ```json
   "coverage": {
     "level": "high|partial|none",
     "best_score": 0.31,
     "results_above_min": 0,
     "suggested_realms": ["Realm A", "Realm B"],
     "hint": "No strong match. Try realm 'X', broader terms, or the digest ~/.memex8/memex8.md."
   }
   ```
2. Level thresholds (configurable `[search.coverage] high = 0.55, partial = 0.40`): high if best ≥ high, partial if best ≥ partial, else none.
3. `suggested_realms`: top 3 realms by cosine between the query vector and realm centroids (`recompute_all_realm_centroids` already maintains these in the `realms` collection).
4. Also compute coverage when results are empty. That's the main case.
5. MCP and Hermes plugin search tools: show coverage on the first line of the text output (e.g. `coverage=none best=0.31 try realms: …`).

**Accept:** a nonsense query → `level=none` with suggested realms. A known-fact query → `level=high`.

---

## Item 5 — Provenance on consolidated summaries

**Idea (from auto-ontology "provenance with every answer"):** every derived memory records what it came from.

**Problem:** `llm_consolidate()` stores summaries with `source_file: None` and nothing linking them to the source memories. Bad merges (e.g. one Oct-4 summary that blended gdm3, sslop.com and apt topics) can't be traced or undone.

**Do:**
1. New `MemoryPoint` fields (all `#[serde(default)]`): `consolidated_from: Vec<String>` (source ids), `derived_by: String` (`"consolidation" | "dedup" | ""`), `consolidated_at: Option<String>`.
2. In `llm_consolidate()`, populate these when storing the summary. That probably means extending `store_memory_with_vector` (`qdrant.rs:692`) with an optional extra-payload map instead of more positional args.
3. If `preserve_originals == false`, the sources get deleted. Archive their `{id, content, ingested_at, memory_type}` into a new `consolidation_archive` Qdrant collection (payload-only, a dummy 1-d vector is fine) or a JSONL file under the data volume, so an un-merge is possible.
4. New REST `POST /api/v1/memories/{id}/unconsolidate`: restore the archived sources (re-embed them), delete the summary, and mark the sources so the next slumber doesn't immediately re-merge them (e.g. `no_consolidate_until` = now + 30 days).
5. Expose `consolidated_from` in `GET /api/v1/memories/{id}` and in Item 1's batch get.

**Accept:** after a slumber, a consolidated memory lists its source ids. Unconsolidate restores them and the next slumber leaves them alone.

---

## Item 6 — User-certified (pinned) memories

**Idea (from auto-ontology "people certify the draft; certified corrections fold back in"):** a human-confirmed memory is protected from automatic rewriting.

**Do:**
1. Add `"certified"` as a value of `verification_status` (existing values: unverified|verified|stale|contradicted), plus fields `certified_at: Option<String>` and `certified_by: Option<String>`.
2. REST: `POST /api/v1/memories/{id}/certify` (optional body `{ "by": "marc", "note": "..." }`) and `DELETE /api/v1/memories/{id}/certify`. MCP + Hermes plugin tool: `memex8_certify(memory_id, revoke=false)`.
3. Effects:
   - Phase 1 dedup: a certified memory always wins and is never deleted.
   - Phase 3b split/merge: it may move realm but is never deleted.
   - Phase 6 consolidation: excluded from consolidation input entirely.
   - Prune / auto-archive / Weibull decay: skipped (treat as no decay).
   - Phase 13 verification sweep: skip (don't downgrade it to stale).
   - `verification_score_multiplier` (`src/engine/mod.rs:49`): certified = 1.05 (small boost).
4. `memex8_remember` gains an optional `certified: bool` so the agent can store and certify in one call when the user explicitly says "remember this".
5. Add a keyword index on `verification_status` if one isn't there.

**Accept:** certify a memory, run slumber twice → it's still present, unchanged, and not merged. Revoke → normal lifecycle resumes.

---

## Suggested order & PRs

1. Item 0 (dedup bug) — `fix/slumber-near-dup-dedup`
2. Items 1 + 2 (compact search, batch get, timeline) — `feat/progressive-disclosure`
3. Item 4 (coverage) — `feat/search-coverage`
4. Items 5 + 6 (provenance, certify) — `feat/provenance-and-certify`
5. Item 3 (`<private>`) — small, can go anywhere; `feat/private-redaction`

For each PR: list new config keys in `config.example.toml`, add an `IMPLEMENTATION_STATUS.md` line, and add tests. Do not deploy to the running container unless told to.

## Deferred (not in this brief)
- "Chill" capture mode for cron auto-sync (store only what would be painful to rediscover)
- Entity synonyms for the knowledge graph (needs entity extraction, still open in TODO.md)
- Dedicated reranker stage (measure after Items 1 and 4 land)
