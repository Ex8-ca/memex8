# PLAN: Memory Upgrades — Verification, Entities, Planner/Worker, Chunked Consolidation

**Status:** Draft
**Date:** 2026-07-16
**Source inspiration:** huytieu/COG-second-brain (patterns only — no code reuse; COG is shell+markdown, memex8 is Rust+Qdrant)
**Prerequisite:** Base build green — `cargo build 2>&1 | grep -c "^error\["` == 0

---

## Overview

Four features, each independently shippable, ordered by value-per-risk:

| Phase | Feature | Value | Risk | Touches |
|-------|---------|-------|------|---------|
| A | Memory verification (memory-hygiene) | Highest | Medium | Schema, slumber, API, recall |
| B | Planner/Worker LLM split (skill distillation) | High (cost) | Low | Config, providers, slumber ph 6 |
| C | Chunked consolidation w/ disk intermediates | Medium | Low | Slumber ph 6 only |
| D | Tiered entity profiles | Medium | Medium | New collection, extraction, API |

**Delegation rule:** One phase per subagent task, build verified after each. Do NOT batch A+B or C+D into one delegation — schema changes and LLM-call-path changes fail differently and must be isolated.

---

## Phase A — Memory Verification (memory-hygiene)

### Concept
memex8 memories age silently. "SABnzbd runs on port 30055" stays in the store at full confidence forever, even after the environment changes. COG's `memory-hygiene` skill periodically re-verifies claims and stamps `last_verified` + confidence. We add the same as a first-class slumber phase plus recall-time weighting.

### Schema changes (`src/storage/qdrant.rs` — `MemoryPoint`)

Add three fields, all `#[serde(default)]` so existing points deserialize cleanly:

```rust
/// ISO-8601 timestamp of last verification sweep. None = never verified.
#[serde(default)]
pub last_verified: Option<String>,
/// Verifier's confidence the claim is still true, 0.0–1.0. None = unverified.
#[serde(default)]
pub verification_confidence: Option<f32>,
/// "unverified" | "verified" | "stale" | "contradicted"
#[serde(default)]
pub verification_status: String,
```

**Pitfall:** Qdrant payload schema is schemaless, so old points just lack the keys — `serde(default)` handles it. No migration needed. But any place that *constructs* `MemoryPoint` literally (ingester, session.rs, tests) must set the new fields — grep for `MemoryPoint {` after the change; the compiler will find them all.

### New slumber phase: Phase 13 "Verification sweep" (`src/engine/slumber.rs`)

Insert after Phase 12 (prune empty realms), before digest write. Runs every slumber (cheap — sampling, not full scan).

**Sampling strategy** (target: ~30 memories per sweep):
1. 40% — highest `access_count` memories never verified (most-relied-upon claims)
2. 40% — oldest `last_verified` in fast-changing realms (`environment`, `technical`, `infrastructure`, `services`)
3. 20% — random sample from everything else

**Verification method — two tracks:**

Track 1 (LLM, always): Batch the sample to the **worker** LLM (see Phase B — until B lands, use the existing provider). Prompt shape:

```
For each claim below, rate confidence it is still true (0.0-1.0).
Rules:
- Facts about running services/ports/paths/versions decay — if older than 90 days, cap at 0.6 unless it's clearly timeless.
- Personal facts (family, preferences, dates of birth) are stable — cap at 0.95.
- If a claim contains an explicit contradiction marker ("no longer", "moved", "replaced"), rate 0.1.
Return JSON: [{"id": "...", "confidence": 0.0-1.0, "reason": "short"}]
```

Map confidence → status: `>=0.8` verified, `0.4–0.8` stale, `<0.4` contradicted.

Track 2 (live checks, deferred to A2 follow-up): regex-extract verifiable primitives from content — IPv4s → ping, `http(s)://host:port` → TCP connect, absolute paths → `Path::exists()`. Only for memories tagged `environment`. **Defer this — LLM-only first, live probes are a separate task with false-positive risk on transient hosts.**

**Write-back:** `set_payload` per memory with the three new fields. Batch into one Qdrant call per 50 points (existing store has batch helpers — check `QdrantStore` for a `set_payload_batch` pattern used by Phase 8 decay).

### Recall integration (`src/engine/mod.rs` — search/recall path)

After TurboVec/Qdrant retrieval, apply a **soft** down-weight:

```rust
let verified_boost = match memory.verification_status.as_str() {
    "verified"     => 1.0,
    "unverified"   => 0.95,   // slight penalty, not exclusion
    "stale"        => 0.85,
    "contradicted" => 0.5,
    _              => 0.95,
};
score *= verified_boost;
```

Never *filter* — Marc would rather see a stale memory flagged than silently missing. The Web UI can badge them later.

### API additions (`src/api/routes/`)

- `GET /api/v1/memories/verification-summary` → `{verified: N, stale: N, contradicted: N, unverified: N}` — cheap scroll over payloads, or track counters in the slumber run and persist to stats.
- Include verification counts in the existing slumber-run report struct so the digest shows them.

### Config (`config.toml`)

```toml
[verification]
enabled = true
sample_size = 30
min_interval_days = 7        # don't re-verify anything verified < 7 days ago
stale_threshold = 0.8        # confidence → status cutoffs
contradicted_threshold = 0.4
```

### Tests
- Unit: confidence→status mapping boundaries (0.8, 0.4 edges)
- Unit: sampling picks high-access + old-verified correctly (mock store)
- Integration: run phase against a seeded Qdrant (existing test harness pattern in slumber tests), assert payloads updated
- Regression: recall ranking of two identical memories, one contradicted → verified ranks first

---

## Phase B — Planner/Worker LLM Split (skill distillation)

### Concept
Today every slumber LLM call (realm naming in 3b, consolidation in 6) uses one provider/model. COG's distillation pattern: expensive model *plans once*, cheap model *executes repeatedly*. We split providers into `planner` (smart: consolidation strategy, realm naming) and `worker` (cheap: batch summarization, verification ratings).

### Config (`config.toml`)

```toml
[llm.planner]
provider = "openai"          # or "ollama"
model = "gpt-4o-mini"        # smart tier — strategy calls
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"

[llm.worker]
provider = "ollama"          # cheap tier — execution calls
model = "qwen2.5:7b"         # local = $0
max_concurrent = 4
```

Backwards compat: if `[llm.planner]` absent, fall back to current single-provider behavior for all calls. **This is the migration path — do not break existing configs.**

### Implementation (`src/engine/providers/`)

- New `PlannerWorkerRouter` in `providers/mod.rs`: holds two `Box<dyn LlmProvider>`, exposes `complete_planner(prompt)` and `complete_worker(prompt)`.
- Call-site audit — grep all `call_openai`/`call_local_llm` (currently in `slumber.rs:1847/1890`):
  - **Planner**: `llm_name_realm` (3b), `merge_similar_realms` decision calls, consolidation *strategy* (new — see below), gap-topic naming (10)
  - **Worker**: verification ratings (Phase A), batch summaries (Phase C), entity extraction (Phase D), session-extraction if cheap enough
- Token accounting: wrap both providers with a counter struct `{prompt_tokens, completion_tokens, calls}` logged per slumber phase into the run report. Cheap to add, immediately shows the cost delta from the split.

### Consolidation strategy split (the actual "distillation")

Phase 6 today: LLM gets all memories in a realm, produces a merged summary — one expensive monolithic call per realm.

New shape:
1. **Planner call** (1 per realm): given memory *titles/summaries only* (not full content — keep the prompt small), output JSON plan: `{merge_groups: [[id,id,...], ...], keep_as_is: [id,...], drop: [id,...]}`
2. **Worker calls** (1 per merge group): given full content of just that group, produce the merged summary.

Result: planner sees ~1/10th the tokens, worker calls are parallelizable, and total spend drops even with a smart planner.

### Tests
- Router falls back to single-provider when `[llm.planner]` absent
- Strategy JSON parse failures → realm is skipped, error logged, slumber continues (never abort the pipeline on one realm)
- Token counters increment and appear in slumber report

---

## Phase C — Chunked Consolidation with Disk-Backed Intermediates

### Concept
Large realms (50+ memories) overflow consolidation prompts; a mid-run failure loses everything. COG's workers write to `/tmp` and return status+path. We do the same inside Phase 6: batch memories, worker-summarize each batch to a temp file, final combine pass, resumable on failure.

### Implementation (`src/engine/slumber.rs` — rework `llm_consolidate`)

```
/var/lib/memex8/tmp/consolidation/{run_id}/{realm_id}/batch-{n}.md
/var/lib/memex8/tmp/consolidation/{run_id}/{realm_id}/manifest.json
```

Flow per realm:
1. Load manifest if exists → skip batches already on disk (**resume**)
2. Chunk realm memories into batches of 10 (config `consolidation.batch_size`)
3. Worker LLM summarizes each batch → write `batch-{n}.md` + update manifest `{completed: [0,1,2...]}`
4. Combine pass (planner): read all batch files, produce final consolidated summary
5. On success: delete the realm's temp dir. On failure: temp dir survives; next slumber resumes.

**Pitfalls:**
- Temp dir is inside the Docker volume (`memex8_data` → `/var/lib/memex8`) — survives container restarts, NOT host-visible. That's correct here (transient state), but document it so nobody goes looking on the host.
- `run_id` must be stable across resume — use `{date}-{realm_id}` not a random UUID, or the manifest lives at a fixed path per realm regardless of run.
- Cap: if a realm's batch count exceeds 20 (200+ memories), consolidate hierarchically — combine batches in groups of 5 first.
- Batch writes must be atomic (`write tmp` + `rename`) — a crash mid-write must not leave a truncated batch file that resume treats as complete.

### Config

```toml
[consolidation]
batch_size = 10
max_batches_flat = 20        # beyond this, hierarchical combine
temp_dir = "/var/lib/memex8/tmp/consolidation"
```

### Tests
- Kill-resume: process 3 of 5 batches, simulate failure, re-run → only batches 4-5 execute (assert worker call count)
- Atomic write: assert no partial batch files after simulated mid-write crash
- Hierarchical: 25-batch realm completes with correct combine depth

---

## Phase D — Tiered Entity Profiles

### Concept
COG's People CRM: profiles escalate Stub(1 mention) → Moderate(3+) → Full(8+). memex8 remembers *facts about* SABnzbd, Deanna, TrueNAS — but never builds a deepening profile per entity. Add an `entities` collection with auto-escalating profiles, fed by mention counting.

### New collection (`src/storage/qdrant.rs`)

```rust
pub struct EntityPoint {
    pub id: String,                  // uuid
    pub name: String,                // canonical: "sabnzbd", "deanna"
    pub entity_type: String,         // "person" | "service" | "project" | "place" | "other"
    pub tier: u8,                    // 3=stub, 2=moderate, 1=full
    pub mention_count: u32,
    pub profile: String,             // LLM-generated, regenerated on tier escalation
    pub source_memory_ids: Vec<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub profile_generated_at: Option<String>,
}
```

Collection `entities`, vector = embedding of `name + profile` (enables "what do I know about X" semantic lookup).

### Extraction (slumber Phase 14, after Phase 13)

Cheap heuristic first, LLM second (worker):
1. **Heuristic pass:** match new memories against existing entity names (exact + case-fold) — increment mention_count, append memory id, update last_seen.
2. **Discovery pass:** worker LLM on memories ingested since last sweep, prompt: "Extract named entities (people, services, projects, places). Return JSON [{name, type}]. Exclude generic tech terms (docker, linux, api)." Cap at 200 memories/sweep.
3. **Dedup/canonicalization:** case-fold + alias map in config (`[entities] aliases = { "sab": "sabnzbd" }`). Merge on name collision.

### Tier escalation

On mention_count crossing thresholds (1 → tier 3 profile, 3 → tier 2, 8 → tier 1):
- **Tier 3 (stub):** name, type, one-line context from first memory. No LLM call — template.
- **Tier 2 (moderate):** worker LLM summarizes all source memories → snapshot paragraph.
- **Tier 1 (full):** planner LLM generates structured profile: `{overview, key_facts[], relationships[], open_questions[]}` — this is where planner quality matters.
- Re-embed entity after each regeneration.

### API (`src/api/routes/entities.rs`)

- `GET /api/v1/entities?tier=1` — list
- `GET /api/v1/entities/{name}` — profile + linked memories (ids only, client can fetch)
- `POST /api/v1/entities/{name}/refresh` — force profile regeneration (planner)
- `DELETE /api/v1/entities/{name}` — for junk entities the discovery pass invents

### Pitfalls
- **Entity explosion:** discovery LLM WILL invent junk ("docker", "the internet", "tuesday"). Mitigate: min 2 mentions before an entity persists; sweep deletes mention_count==1 entities older than 30 days.
- **Canonicalization drift:** "SABnzbd" vs "sabnzbd" vs "Sab" — case-fold on write, alias list for the rest. Log merges so Marc can audit.
- Person entities are sensitive — profiles are local-only (Qdrant), never sent anywhere except the LLM calls that generate them. Note in README.

---

## Build & Verification Protocol (all phases)

1. `cargo build` green before delegation
2. Delegate ONE phase; subagent gets this plan + relevant file paths
3. After return: `cargo build` → `cargo test` → manual smoke (trigger slumber via `docker exec memex8-memex8-1 memex8 slumber trigger`)
4. Commit per phase with `feat(slumber): phase 13 verification sweep` style messages
5. If a phase introduces 20+ compile errors → delegate a "fix all" task rather than piecemeal (established memex8 pattern)

## Rollback

Each phase is feature-flagged in config (`enabled = false` default for A and D; B and C are inert without `[llm.planner]` / keep old path). Worst case: flip flag, rebuild, restart. No data migrations are destructive — new Qdrant fields/collections are additive only.

## Suggested order & effort

| Phase | Effort | Delegate-to |
|-------|--------|-------------|
| A | ~1.5 days equiv | Subagent 1 (schema+phase), Subagent 2 (recall+API) after build check |
| B | ~1 day | Subagent 3 |
| C | ~1 day | Subagent 4 (can parallel with B — different files) |
| D | ~2 days | Subagent 5, after A lands (uses worker LLM from B if available) |

B and C are parallelizable (providers vs slumber.rs internals barely intersect). A must land before D's verification of entity claims; B should land before D's profile generation (planner/worker split pays off there).
