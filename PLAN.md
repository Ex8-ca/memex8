# PLAN.md — memex8 Implementation Plan

> Version: 1.0 | Date: 2026-04-13 | Status: In Progress

## Vision

A self-hosted, Rust-based memory system that gives AI agents persistent, searchable, self-organizing knowledge. Agents ingest context, memories auto-organize into realms, and a background "slumber" process compresses, summarizes, and prunes — all powered by Qdrant vector storage and TurboQuant compression.

## Architecture Layers

```
┌─────────────────────────────────────────────────┐
│ Layer 1: Interfaces                             │
│   CLI (clap) · REST API (Axum) · MCP (stdio)   │
├─────────────────────────────────────────────────┤
│ Layer 2: Engine                                 │
│   Orchestrator · Embedder · Chunker · Ingester  │
│   RealmEngine · SlumberEngine · SearchEngine    │
├─────────────────────────────────────────────────┤
│ Layer 3: Storage                                │
│   Qdrant Client (qdrant-client 1.17)           │
│   3 collections: memories, realms, quantized    │
├─────────────────────────────────────────────────┤
│ Layer 4: Embedding Providers                    │
│   Ollama (local) · OpenAI (cloud) · Extensible  │
└─────────────────────────────────────────────────┘
```

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Vector store | Qdrant | Rust-native, Docker-ready, REST+gRPC API, excellent Rust client |
| Language | Rust | Performance, type safety, small binary (8.4MB stripped) |
| Embeddings | Ollama first, OpenAI fallback | Privacy + cost control, configurable |
| Integration | MCP protocol | Universal — OpenClaw, Hermes, pi.dev all support MCP |
| Chunking | H2 sections by default | Preserves semantic context, configurable |
| Realm assignment | Cosine similarity threshold | Simple, effective, no training needed |
| Compression | TurboQuant (arXiv:2504.19874) | 2.5-3.5 bits/channel, near-zero quality loss |
| Memory importance | upvotes × recency × access_count | Combines explicit + implicit signals |
| Tracing | stderr for MCP compatibility | stdout must be clean JSON-RPC |

## Implementation Phases

### Phase 1: Foundation ✅ (COMPLETE)
- [x] Project scaffold with Cargo.toml
- [x] TOML configuration system
- [x] CLI with 22 commands (clap derive)
- [x] Embedding trait + Ollama + OpenAI providers
- [x] Markdown chunker (section, paragraph, file strategies)
- [x] File ingester with SHA-256 dedup

### Phase 2: Storage Layer ✅ (COMPLETE)
- [x] Qdrant 1.17.0 client integration
- [x] Collection setup (memories, realms, quantized)
- [x] Payload indexes for filtering
- [x] Full CRUD operations
- [x] Vector search with filters
- [x] Scroll API for bulk processing
- [x] Realm management with centroids

### Phase 3: Engine & API ✅ (COMPLETE)
- [x] Engine orchestrator with all operations
- [x] Auto realm assignment by cosine similarity
- [x] Importance-weighted recall with recency decay
- [x] Realm merge with memory reassignment
- [x] Prune queue (low importance + stale)
- [x] Edit memory (re-embed on save)
- [x] Import/Export (JSON)
- [x] REST API (Axum 0.8, 16 routes)
- [x] WebSocket support scaffold

### Phase 4: MCP Integration ✅ (COMPLETE)
- [x] JSON-RPC 2.0 stdio transport
- [x] 11 MCP tool definitions with JSON Schema
- [x] Tool call dispatch to Engine
- [x] Graceful Qdrant fallback
- [x] Initialize + tools/list + tools/call protocol
- [x] Integration generators (Hermes, OpenClaw, pi.dev)

### Phase 5: TurboQuant Compression 🔧 (IN PROGRESS)
- [ ] Lloyd-Max codebook generation (optimal quantization levels)
- [ ] Random orthogonal rotation matrix (Hadamard or QR-decomposed Gaussian)
- [ ] Quantize/dequantize round-trip with accuracy measurement
- [ ] Binary packing of codebook indices (2.5-3.5 bits per dimension)
- [ ] Store quantized vectors in `memories_quantized` collection
- [ ] Slumber pipeline integration

### Phase 6: Slumber Engine 🔧 (PLANNED)
- [ ] Cron-based ingestion scheduler
- [ ] Idle-time detection (time since last query)
- [ ] Re-clustering: k-means on realm memories
- [ ] Realm split (when > split_threshold memories)
- [ ] Realm merge (when cosine similarity > merge_threshold)
- [ ] AAAK-style cluster summarization
- [ ] Prune stale memories (low importance, old, no access)
- [ ] MEMEX8.md write-back to project directories

### Phase 7: Knowledge Graph 🔧 (PLANNED)
- [ ] Entity extraction from memory chunks
- [ ] Relationship detection
- [ ] Graph storage in Qdrant (or separate graph store)
- [ ] Graph traversal API
- [ ] 3D force-directed visualization (Three.js)

### Phase 8: Web UI 🔧 (PLANNED)
- [ ] React SPA with Vite
- [ ] Reddit-like card view for memories
- [ ] Search interface with filters
- [ ] Realm management dashboard
- [ ] 3D knowledge graph view
- [ ] Admin panel (stats, slumber controls, settings)
- [ ] Real-time updates via WebSocket

### Phase 9: Production Readiness 🔧 (PLANNED)
- [ ] Authentication (API key, JWT optional)
- [ ] Rate limiting
- [ ] Backup/restore
- [ ] Multi-user support
- [ ] Metrics (Prometheus)
- [ ] Health check endpoint
- [ ] Docker image publishing
- [ ] CI/CD pipeline

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

### TurboQuant (from arXiv:2504.19874)
```
1. Normalize vector: v_norm = v / ||v||
2. Random rotation: v_rot = R × v_norm (R is orthogonal matrix)
3. For each dimension i:
    index_i = nearest_codebook_index(v_rot[i], codebook)
4. Store: (norm, rotation_seed, indices) → ~3 bits per dimension
```

## File Structure

```
memex8/ (42 Rust files, ~4000 LOC)
├── src/
│   ├── main.rs              # CLI entry
│   ├── config.rs            # Config
│   ├── lib.rs               # Library exports
│   ├── api/                 # REST API (5 files)
│   ├── mcp/                 # MCP server (3 files)
│   ├── engine/              # Core logic (14 files)
│   ├── storage/             # Qdrant (3 files)
│   ├── integrations/        # Integration configs (3 files)
│   └── web/                 # Web UI (2 files, empty)
├── docker-compose.yml
├── Dockerfile
├── Cargo.toml
├── config.example.toml
├── .env.example
├── README.md
├── ARCHITECTURE.md
├── IMPLEMENTATION_STATUS.md
├── PLAN.md                  # This file
└── TODO.md
```
