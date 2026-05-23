# PLAN.md — memex8 Acceleration Plan

> Version: 2.0 | Date: 2026-04-19 | Status: Active
> Target: Hermes Agent Integration (NousResearch/hermes-agent)

## Vision

A self-hosted, Rust-based memory system that gives AI agents persistent, searchable, self-organizing knowledge. Agents ingest context, memories auto-organize into realms, and a background "slumber" process compresses, summarizes, and prunes — all powered by Qdrant vector storage and ScalarQuant compression.

## Architecture Layers

```
┌─────────────────────────────────────────────────┐
│ Layer 1: Interfaces                             │
│   CLI (clap) · REST API (Axum) · MCP (stdio)   │
├─────────────────────────────────────────────────┤
│ Layer 2: Engine                                 │
│   Orchestrator · Embedder · Chunker · Ingester  │
│   RealmEngine · SlumberEngine · SearchEngine    │
│   Watcher · Stream Ingest · Query Cache         │
├─────────────────────────────────────────────────┤
│ Layer 3: Storage                                │
│   Qdrant Client (qdrant-client 1.17)           │
│   3 collections: memories, realms, quantized    │
│   + Knowledge Graph (payload-based)             │
├─────────────────────────────────────────────────┤
│ Layer 4: Embedding Providers                    │
│   Ollama (local) · OpenAI (cloud) · Extensible  │
├─────────────────────────────────────────────────┤
│ Layer 5: Agent Integrations                     │
│   Hermes MCP · OpenClaw · pi.dev               │
│   Migration from Mem0                           │
└─────────────────────────────────────────────────┘
```

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Vector store | Qdrant | Rust-native, Docker-ready, REST+gRPC API, excellent Rust client |
| Language | Rust | Performance, type safety, small binary (8.4MB stripped) |
| Embeddings | Ollama first, OpenAI fallback | Privacy + cost control, configurable |
| Integration | MCP protocol | Universal — Hermes, OpenClaw, pi.dev all support MCP |
| Chunking | H2 sections by default | Preserves semantic context, configurable |
| Realm assignment | Cosine similarity threshold | Simple, effective, no training needed |
| Compression | ScalarQuant (adaptive per-vector range) | 2.5-3.5 bits/channel, near-zero quality loss |
| Memory importance | upvotes × recency × access_count | Combines explicit + implicit signals |
| Tracing | stderr for MCP compatibility | stdout must be clean JSON-RPC |

## Current State (as of 2026-04-19)

### ✅ Complete

- **Core:** Project scaffold, TOML config, 22 CLI commands (clap), embedding trait + Ollama + OpenAI
- **Chunker:** pulldown-cmark AST parser, section/paragraph/file strategies, code block preservation
- **Storage:** Qdrant 1.17.0 integration, 3 collections, payload indexes, full CRUD, scroll API
- **Engine:** 20+ methods (search, store, ingest, recall, realms CRUD, merge, upvote, prune, archive, edit, import/export)
- **REST API:** Axum 0.8, 18 routes, CORS, auth middleware, WebSocket scaffold
- **MCP Server:** JSON-RPC 2.0 stdio, 11 tools, graceful Qdrant fallback, SSE transport scaffold
- **ScalarQuant:** Adaptive scalar quantization, per-vector range, bit-packing, 8 passing tests, 768d benchmark
- **Slumber Engine:** Dedup, compression, realm re-clustering, k-means split, prune flagging, MEMEX8.md write-back
- **Scheduler:** Cron parsing, idle detection, memex8 daemon command
- **Integrations:** Config generators for Hermes, OpenClaw, pi.dev + setup script
- **Web UI:** Embedded SPA in web-dist/ with cards, search, realms, 3D graph
- **Security:** API auth (constant-time Bearer), health endpoint exemption, fail-closed

### ⚠️ Stubbed / TODO

- **File Watcher:** notify crate in Cargo.toml, CLI commands exist, but watch_path/watch_add/watch_list are TODO stubs
- **Backup/Restore:** No export with vectors to tarball, no restore command
- **Batch Ingestion:** Sequential embed calls, no JoinSet parallelism
- **Dynamic Quantization:** Static bit_width config, no access-count/importance-based policy
- **MCP Query Cache:** No prefix caching, no diff responses
- **Stream Integration:** No WebSocket stream endpoint for terminal output
- **Knowledge Graph:** graph_search falls back to semantic search, no entity extraction
- **Prometheus Metrics:** No /metrics endpoint
- **Docker/CI:** Dockerfile and docker-compose exist, no GHCR publishing pipeline
- **Integration Tests:** No testcontainer-based CI tests

---

## Acceleration Phases

### Phase 1: Finish the Foundation (Production-Ready)

**Goal:** Get from 80% to 100% on the existing TODO list

#### 1.1 File Watcher (notify crate)
- Implement `src/engine/watcher.rs` with `RecommendedWatcher`
- Debounce at 500ms after last write event
- SHA-256 compare before re-ingest (skip unchanged files)
- Persist watch config to `config.toml`
- CLI: `memex8 watch add /path` adds to persistent watch list
- `memex8 daemon` auto-starts all configured watchers

#### 1.2 Backup/Restore Commands
- `memex8 backup` — exports all 3 Qdrant collections (memories, realms, quantized) to timestamped tarball with vectors
- `memex8 restore <file>` — reimports from backup, handles ID conflicts
- Store backups in `~/memex8-backups/` with rotation (keep last 7)
- Auto-backup trigger after slumber runs

#### 1.3 Docker Image + GHCR
- Multi-stage Dockerfile (rust:slim builder → debian-slim runtime) — exists, may need updates
- Push to `ghcr.io/{owner}/memex8:latest` via GitHub Actions
- Include Qdrant in docker-compose for one-command deploy — exists, verify health checks
- Health checks on :8080 (REST) and :6333 (Qdrant)

#### 1.4 Integration Tests
- Testcontainer for Qdrant in CI
- Test: ingest file → search → verify result
- Test: realm auto-assignment after 2+ memories
- Test: k-means split when realm exceeds threshold
- Test: import/export roundtrip with vector reuse
- Test: MCP initialize → tools/list → search call

---

### Phase 2: Performance Optimizations (Agent Workload Ready)

**Goal:** Make memex8 fast enough for real agent workloads

#### 2.1 VRAM-Saturated Batch Ingestion

**File:** `src/engine/providers/ollama.rs`

- Replace sequential `embed_chunk()` calls with `tokio::task::JoinSet`
- Bounded worker pool: `Semaphore::new(n)` where n = GPU count × batch_size
- Auto-detect GPU topology: query Ollama `/api/tags` for loaded models, map to GPU assignments
- Batch size heuristic: RTX 5080 (16GB VRAM) → batch of 64 @ 768d, RTX 3090 (24GB) → batch of 96
- Fallback to sequential if Ollama returns 429/503
- Progress reporting during `memex8 ingest` (estimated time remaining, chunks/sec)
- **Expected result:** 1000-file directory from ~45min → ~3min

#### 2.2 Dynamic Quantization Bit-Widths

**File:** `src/engine/quantizer.rs`

- Replace static `quantize_bit_width` config with dynamic policy:
  - `access_count == 0 && importance < 0.3` → 2.0-bit (14.5x compression)
  - `access_count < 5 && importance < 0.5` → 2.5-bit
  - `access_count < 20 && importance < 0.7` → 3.5-bit (current default)
  - `access_count >= 20 || importance >= 0.8` → 4.0-bit (0.93 cosine similarity)
  - `access_count >= 50 || importance >= 0.95` → unquantized (full precision)
- Slumber pipeline re-evaluates bit-widths during nightly runs
- Re-quantize in-place: read full vector from memories collection, apply new bit-width, write to memories_quantized
- Track quantization history in memory payload (`quantization_versions` array)

#### 2.3 MCP Contextual Prefix Caching

**File:** `src/mcp/server.rs`

- Add `QueryCache` struct: LRU cache of `(query_embedding_hash, results_hash, timestamp)`
- On `memex8_search` or `memex8_recall`:
  - Compute query embedding hash
  - If last 3 queries are within cosine similarity threshold (0.95), return diff instead of full payload
  - Diff format: only new memories, removed memories, and updated importance scores
- On `memex8_get`: return full payload (no caching needed for single-ID fetch)
- Cache invalidation: slumber runs, memory edits, new ingests
- **Expected result:** 60-80% reduction in tokens sent over stdio for repeated agent queries

#### 2.4 Termex8 Stream Integration

**File:** `src/api/ingest_stream.rs` (new)

- Add WebSocket endpoint: `POST /api/v1/streams/terminal`
- Termex8 pipes stdout/stderr to memex8 in real-time
- Stream parser: chunks by command boundary (detect `$` prompt patterns)
- Auto-tag with realm "cli-ops" on ingest
- Volatile by default: TTL 7 days, promoted to permanent by slumber if:
  - Contains keywords: "error", "failed", "fix", "workaround", "patch"
  - Upvoted by agent (via API call)
  - Referenced in subsequent searches
- CLI: `memex8 stream start` — starts a persistent stream listener
- CLI: `memex8 stream list` — shows active streams and memory counts

---

### Phase 3: Agent Integration & Migration

**Goal:** Make memex8 the primary memory system, replace Mem0

#### 3.1 Hermes MCP Integration
- Wire memex8 MCP server into Hermes config:
  ```yaml
  mcp_servers:
    memex8:
      transport: stdio
      command: memex8
      args: ["mcp"]
  ```
- Map existing mem0 tool calls to memex8 equivalents:
  - `mcp_mem0_add_memory` → `memex8_store`
  - `mcp_mem0_search_memories` → `memex8_search`
  - `mcp_mem0_get_memories` → `memex8_recall`
  - `mcp_mem0_delete_memory` → `memex8_delete`
- Create migration script: dump all Mem0 memories → import into memex8 with `--reuse-vectors` fallback

#### 3.2 Realm Strategy for Personal Memory
- **personal** — user preferences, communication style, family info
- **environment** — server configs, paths, hardware, services
- **projects** — active development projects (termex8, prism, etc.)
- **troubleshooting** — error workarounds, fixes, debugging notes
- **cli-ops** — terminal output streams, deployment logs (Phase 2.4)
- **conversations** — conversation summaries from OpenClaw/Hermes

#### 3.3 Memory Migration Plan
1. Export all Mem0 memories (all 4 user entities)
2. Clean up: merge duplicates, remove stale/irrelevant entries
3. Import into memex8 with realm auto-assignment
4. Pin personal/environment realms (prevent auto-merge)
5. Test: search for known facts, verify recall accuracy
6. Switch Hermes config from mem0 MCP to memex8 MCP
7. Keep mem0 running as fallback for 2 weeks

---

### Phase 4: Knowledge Graph & Advanced Features

**Goal:** Cross-memory reasoning and entity linking

#### 4.1 Knowledge Graph
- Entity extraction from memory headings, code blocks, and named patterns
- Adjacency list stored in Qdrant payload (`entity_ids`, `related_memory_ids`)
- Graph traversal API: `GET /api/v1/graph/traverse?from=memory-uuid&depth=2`
- MCP tool: `memex8_graph` — find related memories via entity links

#### 4.2 Prometheus Metrics
- `/metrics` endpoint with:
  - `memex8_memories_total` — total memory count
  - `memex8_realms_total` — realm count
  - `memex8_search_latency_seconds` — histogram
  - `memex8_ingest_chunks_total` — counter
  - `memex8_slumber_duration_seconds` — slumber run time
  - `memex8_quantization_ratio` — average compression ratio
  - `memex8_query_cache_hit_rate` — prefix cache effectiveness

---

## Priority Order & Time Estimates

| Phase | Item | Estimated Effort | Priority |
|-------|------|-----------------|----------|
| 1 | File watcher | 2-3 hours | HIGH |
| 1 | Backup/restore | 2 hours | HIGH |
| 2 | Batch ingestion | 4-5 hours | HIGH |
| 2 | Dynamic quantization | 3 hours | HIGH |
| 3 | Hermes integration + migration | 3 hours | HIGH |
| 1 | Docker + GHCR | 2 hours | MEDIUM |
| 2 | MCP prefix caching | 3 hours | MEDIUM |
| 2 | Termex8 stream | 4 hours | MEDIUM |
| 1 | Integration tests | 4-6 hours | MEDIUM |
| 4 | Knowledge graph | 6-8 hours | LOW |
| 4 | Prometheus metrics | 2 hours | LOW |

**Total: ~35-40 hours of focused work**

---

## Key Algorithms

### Realm Assignment

```
for each new memory vector v:
    for each realm r with centroid c:
        score = cosine_similarity(v, c)
    if max_score >= threshold:
        assign to realm with max score
    else:
        create new realm with centroid = v
```

### Recall Scoring

```
score = importance × recency × (1 + access_count × 0.05)
where recency = 1 / (1 + days_since_access × 0.1)
```

### ScalarQuant (Adaptive Scalar Vector Quantization)

```
1. Normalize vector: v_norm = v / ||v||
2. Find per-coordinate min/max over actual vector values
3. Uniform quantization within that range → pack bits
4. Store: (norm, min, max, packed_indices) → ~3 bits per dimension
```

---

## Hermes Integration Reference

### MCP Tool Mapping (mem0 → memex8)

| mem0 Tool | memex8 Tool | Notes |
|-----------|-------------|-------|
| `mcp_mem0_add_memory` | `memex8_store` | Add realm_hint support |
| `mcp_mem0_search_memories` | `memex8_search` | Add min_score + realm filter |
| `mcp_mem0_get_memories` | `memex8_recall` | Importance-weighted |
| `mcp_mem0_delete_memory` | `memex8_delete` | Force flag available |

### Quick Hermes Config

```bash
# Generate config to paste into Hermes
memex8 integration hermes

# Or use the setup script
./scripts/setup-hermes.sh
```

---

## File Structure

```
memex8/ (45 Rust files, ~4500 LOC)
├── src/
│   ├── main.rs                  # CLI entry (22 commands)
│   ├── config.rs                # TOML config system
│   ├── lib.rs                   # Library exports
│   ├── api/                     # REST API (8 files)
│   │   ├── server.rs            # Axum server
│   │   ├── auth.rs              # Bearer token middleware
│   │   ├── error.rs             # Error handling
│   │   ├── routes/              # Route handlers
│   │   │   ├── memories.rs
│   │   │   ├── realms.rs
│   │   │   ├── search.rs
│   │   │   ├── slumber.rs
│   │   │   ├── stats.rs
│   │   │   ├── webhook.rs
│   │   │   └── websocket.rs
│   │   └── ingest_stream.rs     # NEW: terminal stream (Phase 2.4)
│   ├── mcp/                     # MCP server (5 files)
│   │   ├── mod.rs
│   │   ├── server.rs            # stdio + SSE transport
│   │   ├── tools.rs             # 11 tool definitions
│   │   ├── transport.rs
│   │   └── http.rs
│   ├── engine/                  # Core logic (14 files)
│   │   ├── mod.rs               # Engine orchestrator
│   │   ├── chunker.rs           # Markdown chunker (pulldown-cmark)
│   │   ├── compressor.rs
│   │   ├── doctor.rs            # Diagnostics
│   │   ├── embedder.rs          # Embedding trait
│   │   ├── graph.rs             # Knowledge graph (stub)
│   │   ├── ingester.rs          # File/directory ingestion
│   │   ├── memex8_md.rs         # Project write-back
│   │   ├── providers/           # Embedding providers
│   │   │   ├── mod.rs
│   │   │   ├── ollama.rs        # Ollama /api/embed (Phase 2.1 target)
│   │   │   └── openai.rs
│   │   ├── quantizer.rs         # ScalarQuant (Phase 2.2 target)
│   │   ├── realms.rs
│   │   ├── scheduler.rs         # Cron + idle detection
│   │   ├── search.rs
│   │   ├── slumber.rs           # Background pipeline
│   │   └── watcher.rs           # NEW: file watcher (Phase 1.1)
│   ├── storage/                 # Qdrant (3 files)
│   │   ├── mod.rs
│   │   ├── qdrant.rs            # Qdrant client wrapper
│   │   └── migrations.rs
│   ├── integrations/            # Agent integrations (3 files)
│   │   ├── mod.rs
│   │   ├── hermes.rs            # Hermes config generator
│   │   ├── openclaw.rs          # OpenClaw webhook config
│   │   └── pi.rs                # pi.dev extension
│   └── web/                     # Embedded web UI (2 files)
│       ├── mod.rs
│       └── embedded.rs
├── scripts/
│   └── setup-hermes.sh          # Hermes integration setup
├── plugins/                     # Hermes plugin
│   └── memex8/
├── web-dist/                    # Pre-built web UI
│   └── index.html
├── docker-compose.yml
├── Dockerfile
├── Cargo.toml
├── Cargo.lock
├── config.example.toml
├── .env.example
├── .dockerignore
├── .gitignore
├── README.md
├── ARCHITECTURE.md
├── IMPLEMENTATION_STATUS.md
├── TODO.md
└── PLAN.md                      # This file
```

---

---

## Completed Work Summary (2026-05-22)

### Since PLAN.md v2.0, these items are now DONE:

- ✅ **Backup/Restore** (`src/engine/backup.rs` — 283 lines) — tarball export with vectors, restore, backup listing
- ✅ **Knowledge Graph** (`src/engine/graph.rs` — 531 lines expanded) — entity extraction, adjacency lists, traversal API
- ✅ **Quantizer improvements** (`src/engine/quantizer.rs` — 81 new lines) — enhanced ScalarQuant, dynamic policies
- ✅ **Ollama provider** (`src/engine/providers/ollama.rs` — 169 new lines) — configurable base_url, OpenAI-compatible
- ✅ **Slumber enhancements** (`src/engine/slumber.rs` — 157 new lines) — consolidation, gap detection, realm naming
- ✅ **API routes** — graph, inference, slumber, webhook, websocket routes added
- ✅ **MCP tools** — new tools for graph, backup, inference operations
- ✅ **Docker hardening** — multi-stage build, healthcheck fix, env var flow from `.env`
- ✅ **CI/CD** — GitHub Actions (ci.yml, docker-release.yml), pushed to GitLab + GitHub
- ✅ **Engine orchestrator** (`src/engine/mod.rs` — 56 new lines) — associations, reactions, session modules
- ✅ **Reactions** (`src/engine/reactions.rs`) — emotional/engagement scoring for importance boost
- ✅ **Session extraction** (`src/engine/session.rs`) — session-end memory extraction
- ✅ **Inference routes** (`src/api/routes/inference.rs`) — proactive suggestion and gap resolution API
- ✅ **Qdrant storage** (`src/storage/qdrant.rs` — 195 new lines) — expanded client methods
- ✅ **Config system** (`src/config.rs` — 47 new lines) — new config options, OpenAI-compatible provider support
- ✅ **Docker healthcheck** — fixed from `/api/v1/stats` (401) to `/api/v1/health` (200)
- ✅ **memex8-update skill** — reusable workflow for future updates

### Still Remaining (tracked in code as TODOs):

See Phase 5 below for actionable next steps.

---

## Phase 5: Remaining TODOs + Future Enhancements (2026-05-22+)

**Priority: Fix existing bugs first, then expand capabilities.**

### 5.1 File Watcher Activation (HIGH — 2-3 hours)

**Current state:** `watcher.rs` exists (391 lines) but `ingester.rs:78` still has `// TODO: use notify crate for real-time file watching`

**What to do:**
- Wire up `watcher.rs` into the ingestion pipeline so `memex8 daemon` actually monitors configured paths
- Test: modify a watched file → verify auto-ingest triggers
- Add SHA-256 dedup to skip unchanged files on re-ingest

### 5.2 Realm Centroid Recomputation (HIGH — 1-2 hours)

**Current state:** `realms.rs:71-93` — centroid computation, k-means split, and realm merging are all TODO stubs. Slumber falls back to basic heuristics.

**What to do:**
- Implement `recompute_centroids()` — fetch all member vectors per realm, compute mean, update
- Implement `split_large_realms()` — k-means k=2 on realm members, check cluster distance threshold
- Implement `merge_similar_realms()` — compare centroid pairs, merge below threshold
- This will dramatically improve realm organization quality

### 5.3 Knowledge Graph Traversal (MEDIUM — 3-4 hours)

**Current state:** `engine/mod.rs:970` — `TODO: implement knowledge graph traversal`. Graph module exists (531 lines) but traversal falls back to semantic search.

**What to do:**
- Wire `graph_search` to use actual adjacency list traversal instead of semantic fallback
- Add `GET /api/v1/graph/traverse?from=<memory_id>&depth=2` REST endpoint
- Add MCP tool `memex8_graph_traverse` for agent access
- Test: store related memories → verify graph links are traversable

### 5.4 MCP SSE Transport (MEDIUM — 2 hours)

**Current state:** `mcp/server.rs:328` — `TODO: implement SSE transport using Axum`

**What to do:**
- Implement SSE (Server-Sent Events) transport using `axum::response::sse`
- Allows remote agents to connect over HTTP instead of stdio-only
- Required for running memex8 as a service accessed by multiple agents

### 5.5 WebSocket Event Broadcasting (LOW — 2 hours)

**Current state:** `api/routes/websocket.rs:17` — `TODO: broadcast events from slumber/ingester`

**What to do:**
- Broadcast slumber completion, ingest progress, and realm changes to WebSocket subscribers
- Enable the Web UI to show real-time updates without polling

### 5.6 Init Interactive Wizard (LOW — 2 hours)

**Current state:** `main.rs:329` — `TODO: interactive wizard` for `memex8 init`

**What to do:**
- Interactive CLI prompts for: embedding provider, Qdrant URL, auth key, watch paths, slumber schedule
- Generate `config.toml` from wizard answers
- Use `dialoguer` crate for prompts

### 5.7 Missing "quantized" Qdrant Collection (BUG — 30 min)

**Current state:** Slumber warns: `Collection 'quantized' doesn't exist!` every run. Harmless but noisy.

**What to do:**
- Either create the `quantized` collection during init/doctor, or
- Make the slumber pipeline skip quantized optimization if collection doesn't exist (graceful handling)

### 5.8 Mem0 Migration Tool (MEDIUM — 3 hours)

**Current state:** Not started. Mem0 is still Hermes's primary memory.

**What to do:**
- `memex8 migrate mem0` — export all Mem0 memories via MCP, import into memex8
- Auto-assign realms based on content similarity to existing realms
- Pin important realms (personal, environment) to prevent auto-merge
- Test: search for known facts → verify recall accuracy matches Mem0

### 5.9 Realm Pinning (LOW — 1 hour)

**Current state:** No pinning mechanism. Slumber can merge/rename any realm.

**What to do:**
- Add `is_pinned: bool` to realm payload
- `memex8 realms pin <realm_id>` / `unpin`
- Slumber skips pinned realms during merge/split/rename
- Critical for `personal`, `environment`, and `cli-ops` realms

### 5.10 API Documentation / OpenAPI Spec (LOW — 2 hours)

**Current state:** No API docs.

**What to do:**
- Add `utoipa` annotations to Axum routes
- Generate OpenAPI spec at `GET /api/v1/openapi.json`
- Serve Swagger UI at `GET /api/v1/docs`
- Helps agents and humans understand available endpoints

### 5.11 Web UI Polish (LOW — 4 hours)

**Current state:** Basic SPA in `web-dist/` with cards, search, realms, 3D graph.

**What to do:**
- Dark/light mode toggle
- Memory edit inline (call `PUT /api/v1/memories/:id`)
- Realm management UI (pin, merge, rename)
- Backup/restore UI
- Real-time updates via WebSocket (Phase 5.5)

### 5.12 Performance Profiling + Benchmarks (MEDIUM — 3 hours)

**Current state:** No benchmarks.

**What to do:**
- Add `criterion` benchmarks for: search latency, ingest throughput, slumber duration
- Profile memory usage at 10K, 50K, 100K memories
- Document expected performance targets
- Add `memex8 bench` CLI command

---

## Total Remaining Effort Estimate

| Item | Priority | Hours |
|------|----------|-------|
| 5.1 File watcher activation | HIGH | 2-3 |
| 5.2 Realm centroid recomputation | HIGH | 1-2 |
| 5.3 Knowledge graph traversal | MEDIUM | 3-4 |
| 5.4 MCP SSE transport | MEDIUM | 2 |
| 5.5 WebSocket broadcast | LOW | 2 |
| 5.6 Init wizard | LOW | 2 |
| 5.7 Missing quantized collection | BUG | 0.5 |
| 5.8 Mem0 migration | MEDIUM | 3 |
| 5.9 Realm pinning | LOW | 1 |
| 5.10 OpenAPI docs | LOW | 2 |
| 5.11 Web UI polish | LOW | 4 |
| 5.12 Benchmarks | MEDIUM | 3 |

**Total: ~25-30 hours**

---

*Last updated: 2026-05-22*
