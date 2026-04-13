# TODO.md — memex8 Action Items

> Last updated: 2026-04-13

## 🔴 High Priority (Next Session)

### 1. TurboQuant Implementation
- [ ] **Lloyd-Max codebook generator** — Generate optimal 256-level codebook for uniform distribution
  - File: `src/engine/quantizer.rs`
  - Algorithm: iterative refinement of initial linear codebook
- [ ] **Random orthogonal rotation** — Implement proper rotation matrix
  - Option A: Hadamard transform (fast, deterministic)
  - Option B: QR decomposition of random Gaussian matrix (true random)
  - Store rotation seed in quantized payload for reproducibility
- [ ] **Quantize → Dequantize round-trip** — Verify accuracy
  - Test: cosine similarity before/after should be > 0.95
  - Target: 2.5-3.5 bits per dimension
- [ ] **Binary packing** — Pack indices into compact binary format
  - 8-bit indices → 256 levels → 8 bits/dim (baseline)
  - Can compress further with entropy coding if needed

### 2. Slumber Engine Pipeline
- [ ] **File: `src/engine/slumber.rs`** — Implement `run_full_pipeline()`
  - [ ] Phase 1: Quantize all memories using TurboQuant
  - [ ] Phase 2: Re-cluster realms (k-means on memory vectors)
  - [ ] Phase 3: Merge realms with high cosine similarity
  - [ ] Phase 4: Summarize clusters (AAAK-style, LLM-assisted)
  - [ ] Phase 5: Prune stale memories
  - [ ] Phase 6: Update MEMEX8.md files
- [ ] **Cron scheduler** — Parse cron expressions, run at intervals
  - Use `cron` crate for parsing
  - `tokio::time::interval` for execution
- [ ] **Idle detection** — Track last query time, trigger when idle
  - Already tracking `last_query` in slumber state
  - Compare with `slumber.idle_timeout`

### 3. File Watcher
- [ ] **File: `src/engine/ingester.rs`** — Implement `start_watching()`
  - Use `notify` crate for real-time file system events
  - Debounce events (avoid re-ingesting during writes)
  - Only re-ingest changed files (SHA-256 comparison)
- [ ] **Persist watch config** — Save to config.toml or separate JSON
  - `memex8 watch add <path> --chunk-by section --poll 5m`
  - Load watches on startup

## 🟡 Medium Priority

### 4. MEMEX8.md Write-Back
- [ ] **File: `src/engine/memex8_md.rs`** — Implement `write_memex8_md()`
  - Generate markdown file with top N memories by importance
  - Format: heading + content + metadata (score, realm, source)
  - Write to project directories based on source_file paths
  - Update on slumber completion
- [ ] **Configure max memories** — `memex8_md.max_memories` (default: 20)

### 5. Knowledge Graph
- [ ] **File: `src/engine/graph.rs`** — Implement entity extraction
  - Parse headings, named entities, code symbols
  - Build adjacency list: entity → [related_entities]
  - Store in Qdrant as payload or separate graph store
- [ ] **Graph search API** — Traverse relationships up to depth N
  - Already scaffolded in `engine.graph_search()`
  - Currently falls back to semantic search

### 6. Import Re-embedding
- [ ] **Fix: `engine.import()`** — Currently re-embeds every memory
  - For imported JSON, vectors are already computed
  - Add option: `--reuse-vectors` to skip re-embedding
  - Requires storing vectors in export JSON

### 7. API Enhancements
- [ ] **Authentication middleware** — Bearer token validation
  - File: `src/api/auth.rs`
  - Read API key from config, compare with `Authorization` header
- [ ] **Paginated search** — Add `offset` parameter
- [ ] **Filter by tags** — Add `?tags=tag1,tag2` to search
- [ ] **Health endpoint** — Already exists at `/health`, add detailed `/healthz`

## 🟢 Low Priority

### 8. Web UI
- [ ] React SPA with Vite
- [ ] Card-based memory browser
- [ ] Search interface
- [ ] 3D force-directed knowledge graph (Three.js)
- [ ] Realm management dashboard
- [ ] Settings page
- [ ] Embed static assets into binary with `include_dir`

### 9. Testing
- [ ] Unit tests for chunker (section, paragraph, file strategies)
- [ ] Unit tests for quantizer (round-trip accuracy)
- [ ] Integration tests with testcontainers (Qdrant)
- [ ] API integration tests with `axum::test`

### 10. Documentation
- [ ] API reference (OpenAPI/Swagger spec)
- [ ] MCP tool documentation
- [ ] Tutorial: "Setting up memex8 with Hermes"
- [ ] Tutorial: "Ingesting your knowledge base"

### 11. Performance
- [ ] Batch embedding requests (reduce API calls)
- [ ] Connection pooling for Qdrant
- [ ] Caching for frequent searches
- [ ] Memory-mapped file reading for large files

### 12. Production
- [ ] Docker image publishing (GitHub Container Registry)
- [ ] CI/CD pipeline (GitHub Actions)
- [ ] Prometheus metrics
- [ ] Backup/restore procedures
- [ ] Multi-user support with access control

## ✅ Completed

- [x] Project scaffold with Cargo.toml
- [x] TOML configuration system
- [x] CLI with 22 commands (clap)
- [x] Embedding trait + Ollama + OpenAI providers
- [x] Markdown chunker (section, paragraph, file)
- [x] File ingester with SHA-256 dedup
- [x] Qdrant 1.17.0 storage layer (full CRUD, search, scroll)
- [x] Collection setup with payload indexes
- [x] Engine orchestrator (20+ methods)
- [x] Auto realm assignment by cosine similarity
- [x] Importance-weighted recall with recency decay
- [x] Realm merge with memory reassignment
- [x] Prune queue (low importance + stale)
- [x] Edit memory (open $EDITOR, re-embed)
- [x] Import/Export (JSON)
- [x] REST API (Axum 0.8, 16 routes)
- [x] MCP server (JSON-RPC 2.0 stdio, 11 tools)
- [x] Integration generators (Hermes, OpenClaw, pi.dev)
- [x] Doctor diagnostics
- [x] Tracing to stderr (MCP compatibility)
- [x] Docker Compose setup
- [x] Dockerfile (multi-stage build)
- [x] Compiles clean, 8.4MB stripped binary
