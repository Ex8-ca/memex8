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

*Last updated: 2026-04-19*
