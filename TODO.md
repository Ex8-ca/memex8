# TODO.md — memex8 Action Items

> Last updated: 2026-04-13

## 🔴 High Priority

### 1. TurboQuant: Real Compression Pipeline
- [x] **Store vectors in Qdrant** — `scroll_all_memories_with_vectors()` fetches vectors from Qdrant
  - File: `src/storage/qdrant.rs` — `extract_vector()` handles dense/sparse/multi-vector extraction
  - Slumber now compresses real vectors, not placeholders
- [x] **Binary packing** — Tight bit-packing for 2.5/3.0/3.5-bit quantization
  - `pack_bits()` / `unpack_bits()` functions in `src/engine/quantizer.rs`
  - 768d @ 3.5-bit: 384 bytes packed / 404 bytes total (7.6x compression from 3072 bytes)
- [x] **Benchmark at 768d with real embeddings** — 11 tests pass including full 768d benchmark
  - 768d results: 2.0-bit→cos0.79, 2.5-bit→cos0.81, 3.0-bit→cos0.81, 3.5-bit→cos0.90, 4.0-bit→cos0.93

### 2. Auto-Realm Assignment + Split
- [x] **Store real realm centroids** — `compute_realm_centroid()` + `recompute_all_realm_centroids()`
  - Recomputed during slumber from actual memory vectors
- [x] **k-means realm split** — `split_large_realms()` in slumber phase 3b
  - 2-means on realm memory vectors, creates two new realms with split membership
  - Respects user-pinned realms, minimum cluster size of 5

### 3. Slumber: Cron Scheduler + Idle Trigger
- [x] **Cron scheduler** — `src/engine/scheduler.rs` parses `*/N * * * *` expressions
- [x] **Idle detection** — tracks last query time, auto-triggers slumber after timeout
- [x] **`memex8 daemon`** — new CLI command to start background scheduler

## 🟡 Medium Priority

### 4. API Authentication
The middleware skeleton exists but isn't wired.

- [ ] **Bearer token auth** — validate `Authorization: Bearer <key>` on all routes
  - File: `src/api/auth.rs` — implement actual key comparison
  - File: `src/api/server.rs` — add `.layer(auth_middleware)` to routes
  - Add `.route("/health", ...)` as excluded path

### 5. Search Enhancements
- [ ] **Filter by tags** — add `?tags=tag1,tag2` query param to search
  - Already supported in Qdrant (tag index exists), just need API wiring
- [ ] **Paginated results** — add `offset` parameter
- [ ] **Tag suggestions** — endpoint to list most-used tags across memories

### 6. Chunker: Better Markdown Parsing
Current chunker uses simple line-by-line parsing. Would be more robust with `pulldown-cmark`.

- [ ] **Use pulldown-cmark AST** — parse headings properly (H1-H6 hierarchy), detect code blocks, tables, lists
  - Preserve code blocks as single chunks (don't split in the middle of code)
  - Track parent heading chain (H1 > H2 > H3) for context
  - File: `src/engine/chunker.rs`

### 7. Import with Vector Reuse
- [ ] **`--reuse-vectors` flag** — for imported JSON, skip re-embedding if vectors are stored
  - Add `vector: Option<Vec<f32>>` to `MemoryPoint` export format
  - File: `src/engine/mod.rs` — `import()` method

## 🟢 Low Priority

### 8. File Watcher
- [ ] **Real-time directory watching** — `notify` crate integration
  - Debounce (500ms after last write event)
  - Only re-ingest changed files (SHA-256 comparison)
  - Persist watch config

### 9. Knowledge Graph
- [ ] **Entity extraction from headings/code** — parse entity names from memory content
  - Build adjacency list, store in Qdrant payload
  - Graph traversal API

### 10. Web UI
- [ ] React SPA with card view, search, realm management
- [ ] 3D force-directed graph (Three.js)
- [ ] Embed in binary with `include_dir`

### 11. Testing
- [ ] Chunker tests (section, paragraph, file strategies with real markdown)
- [ ] Integration tests with Qdrant testcontainer
- [ ] API integration tests
- [ ] TurboQuant benchmarks at 768d (the actual embedding dimension)

### 12. Production
- [ ] GitHub Actions CI (build, test, clippy)
- [ ] Docker image to GHCR
- [ ] Backup/restore (`memex8 backup` / `memex8 restore`)
- [ ] Prometheus metrics endpoint

## ✅ Completed

### Core
- [x] Project scaffold + Cargo.toml (25+ deps)
- [x] TOML configuration system with defaults
- [x] CLI with 22 commands (clap derive)
- [x] Embedding trait + Ollama (`/api/embed`) + OpenAI providers
- [x] Markdown chunker (section, paragraph, file strategies)
- [x] File ingester with SHA-256 dedup + walkdir

### Storage
- [x] Qdrant 1.17.0 full integration (680 LOC)
- [x] 3 collections: memories, realms, memories_quantized
- [x] Payload indexes (realm_name, tags, chunk_type, importance)
- [x] Full CRUD + vector search + scroll API
- [x] Realm management with create/find-by-name/delete

### Engine
- [x] Engine orchestrator (20+ methods)
- [x] Auto realm assignment by cosine similarity
- [x] Importance-weighted recall (importance × recency × access_count)
- [x] Realm merge with memory reassignment
- [x] Prune queue (low importance + stale detection)
- [x] Edit memory (open $EDITOR, re-embed on save)
- [x] Import/Export (JSON)

### Interfaces
- [x] REST API (Axum 0.8, 16 routes, CORS, tracing)
- [x] MCP server (JSON-RPC 2.0 stdio, 11 tools, graceful fallback)
- [x] Integration generators (Hermes MCP config, OpenClaw webhooks, pi.dev extension)
- [x] Doctor diagnostics (Qdrant, Ollama/OpenAI, config)

### TurboQuant
- [x] Lloyd-Max codebook (Beta distribution, 50K samples, 50 iterations)
- [x] Random orthogonal rotation (QR via Gram-Schmidt)
- [x] Round-trip quantize/dequantize with quality reporting
- [x] 11 passing tests (including 768d benchmark, bit-packing, compression ratio)
- [x] Binary bit-packing: 768d @ 3.5-bit = 384 bytes packed (7.6x compression)
- [x] Vectors stored in Qdrant, fetched by slumber for real compression
- [x] **768d benchmark results**: 2.0→0.79, 2.5→0.81, 3.0→0.81, 3.5→0.90, 4.0→0.93

### Slumber Engine
- [x] Phase 1: Deduplication (hash-based)
- [x] Phase 2: TurboQuant compression (quality gated, real vectors from Qdrant)
- [x] Phase 3: Realm re-clustering (count updates + centroid recomputation from real vectors)
- [x] Phase 3b: Realm split (k-means k=2, respects user-pinned, min cluster size 5)
- [x] Phase 4: Prune flagging (age × importance × access scoring)
- [x] Phase 5: MEMEX8.md write-back (top N memories per directory)
- [x] SlumberReport with full metrics tracking
- [x] Cron/idle scheduler (`memex8 daemon` command)
- [x] Activity tracking on queries

### Security
- [x] API authentication middleware (constant-time Bearer token comparison)
- [x] Health endpoint exemption from auth
- [x] Fail-closed when no API key configured

### DevOps
- [x] Docker Compose (Qdrant + memex8 + optional Ollama)
- [x] Multi-stage Dockerfile (8.4MB stripped binary)
- [x] LICENSE (MIT), README.md, PLAN.md
