# memex8 — Architecture Specification

> A Rust-based, self-hosted AI memory system with Qdrant vector storage, ScalarQuant-aware compression, auto-discovered knowledge realms, and integrations for OpenClaw, Hermes Agent, and pi.dev.

## Table of Contents

- [Overview](#overview)
- [Architecture Diagram](#architecture-diagram)
- [Core Components](#core-components)
- [Memory Model](#memory-model)
- [Realm System](#realm-system)
- [Slumber Engine](#slumber-engine)
- [Embedding Providers](#embedding-providers)
- [ScalarQuant Integration](#scalarquant-integration)
- [CLI Reference](#cli-reference)
- [REST API](#rest-api)
- [MCP Server](#mcp-server)
- [Integration Hooks](#integration-hooks)
- [Web UI](#web-ui)
- [Docker Deployment](#docker-deployment)
- [Configuration](#configuration)
- [Directory Structure](#directory-structure)
- [Roadmap](#roadmap)

---

## Overview

memex8 gives AI agents long-term, structured memory. It ingests `.md` files and directories, embeds them into vectors, stores them in Qdrant, and organizes them into auto-discovered **realms** of knowledge. A **slumber** process runs during idle time to re-quantize, re-cluster, summarize, and prune memories — inspired by ScalarQuant's near-optimal vector quantization.

**Key principles:**
- **Local-first, self-hosted** — runs in Docker on your machine
- **Embedding-flexible** — local (Ollama/nomic-embed) or cloud (OpenAI)
- **Augment, don't replace** — writes `MEMEX8.md` back to project dirs for model pickup
- **Realm auto-discovery** — memories self-organize; users can pin/promote realms
- **Slumber** — idle-time maintenance: compress, re-cluster, summarize, prune

### Target Integrations

| Platform | Method | Purpose |
|----------|--------|---------|
| **OpenClaw** | Webhook hooks + REST CLI | Ingest on events, search via API |
| **Hermes Agent** | MCP server (stdio/HTTP) | Native tool access to memory |
| **pi.dev** | Extension + skill files | Custom tools (`memex8_search`, `memex8_store`) |

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         DOCKER COMPOSE                              │
│                                                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │   Qdrant      │  │   memex8     │  │      Web UI              │  │
│  │   (6333)      │  │   Core       │  │  (3000)                  │  │
│  │               │  │   (8080)     │  │                          │  │
│  │  Collections: │  │              │  │  ┌──────────────────┐    │  │
│  │  - memories   │◄─┤  REST API    │──►│  │ Reddit-like      │    │  │
│  │  - realms     │  │  MCP Server  │  │  │ Memory Browser   │    │  │
│  │  - quantized  │  │  Slumber     │  │  │ (upvote/prune)   │    │  │
│  │               │  │  Ingester    │  │  └──────────────────┘    │  │
│  └──────────────┘  │  CLI         │  │  ┌──────────────────┐    │  │
│                     │              │  │  │ 3D Force Graph   │    │  │
│  ┌──────────────┐  │  Embedding   │  │  │ (Three.js)       │    │  │
│  │  Ollama      │  │  Providers:  │  │  └──────────────────┘    │  │
│  │  (11434)     │◄─┤  - Ollama    │  │  ┌──────────────────┐    │  │
│  │  (optional)  │  │  - OpenAI    │  │  │ Admin Dashboard  │    │  │
│  │              │  │              │  │  │ (stats/slumber)  │    │  │
│  └──────────────┘  └──────┬───────┘  └──────────────────────────┘  │
│                           │                                        │
└───────────────────────────┼────────────────────────────────────────┘
                            │
            ┌───────────────┼───────────────┐
            │               │               │
     ┌──────▼──────┐ ┌─────▼──────┐ ┌─────▼──────┐
     │  OpenClaw    │ │  Hermes    │ │  pi.dev    │
     │  (webhooks)  │ │  (MCP)     │ │ (extension)│
     └─────────────┘ └────────────┘ └────────────┘
```

---

## Core Components

### 1. Storage Layer — Qdrant

Three Qdrant collections:

| Collection | Purpose | Vector Config |
|-----------|---------|---------------|
| `memories` | Full-resolution embeddings + payload | 768d (nomic) or 1536d (OpenAI small), cosine |
| `realms` | Realm centroid vectors + metadata | Same dim as memories |
| `quantized` | ScalarQuant-compressed versions for fast ANN | Reduced bit-width (configurable 2.5-4 bits/channel) |

**Payload schema for a memory point:**

```json
{
  "id": "uuid",
  "vector": [0.1, 0.2, ...],
  "payload": {
    "content": "Original markdown text",
    "summary": "AAAK-compressed summary (~50 tokens)",
    "source_file": "/path/to/file.md",
    "source_hash": "sha256",
    "realm_id": "realm-uuid",
    "realm_name": "rust-patterns",
    "importance": 0.85,
    "upvotes": 3,
    "tags": ["rust", "async", "patterns"],
    "entity_ids": ["ent-123", "ent-456"],
    "ingested_at": "2026-04-13T10:00:00Z",
    "last_accessed": "2026-04-13T15:30:00Z",
    "access_count": 12,
    "slumber_version": 3,
    "chunk_type": "section",
    "heading": "## Async Patterns",
    "metadata": {}
  }
}
```

### 2. Memory Engine (Rust — Axum-based)

The core binary handles everything:

```
memex8
├── api/            — REST + WebSocket server (Axum)
├── mcp/            — MCP server (stdio + SSE)
├── engine/
│   ├── ingester/   — File watcher, .md parser, chunker
│   ├── embedder/   — Provider abstraction (Ollama, OpenAI)
│   ├── realms/     — Auto-discovery, clustering, splitting
│   ├── slumber/    — Idle detection, maintenance pipeline
│   ├── quantizer/  — ScalarQuant-inspired compression
│   ├── compressor/ — AAAK-style summarization
│   ├── search/     — Semantic search with realm filters
│   └── graph/      — Entity extraction, relationship tracking
├── integrations/
│   ├── openclaw/   — Webhook handlers, hook scripts
│   ├── hermes/     — MCP tool definitions
│   └── pi/         — Extension/skill generators
├── web/            — Embedded static assets for Web UI
└── config/         — TOML configuration parsing
```

### 3. Embedding Providers

Provider abstraction with runtime selection:

```toml
[embedding]
provider = "ollama"          # "ollama" | "openai"
model = "nomic-embed-text"   # or "text-embedding-3-small"

[embedding.ollama]
url = "http://ollama:11434"

[embedding.openai]
api_key_env = "OPENAI_API_KEY"
model = "text-embedding-3-small"
```

**Supported providers (v1):**

| Provider | Model | Dimensions | Cost | Local? |
|----------|-------|-----------|------|--------|
| Ollama | nomic-embed-text-v1.5 | 768 | Free | Yes |
| Ollama | mxbai-embed-large | 1024 | Free | Yes |
| OpenAI | text-embedding-3-small | 1536 | $0.02/1M tokens | No |
| OpenAI | text-embedding-3-large | 3072 | $0.13/1M tokens | No |

Users pick at init. All memories in a single installation use the same model for consistency.

---

## Memory Model

### Ingestion Pipeline

```
.md File
   │
   ▼
┌──────────┐    ┌───────────┐    ┌──────────┐    ┌──────────┐
│  Parse   │───►│  Chunk    │───►│  Embed   │───►│  Store   │
│  (md)    │    │  (section)│    │  (API)   │    │ (Qdrant) │
└──────────┘    └───────────┘    └──────────┘    └──────────┘
                     │                                  │
                     ▼                                  ▼
              ┌──────────┐                       ┌──────────┐
              │ Metadata │                       │ Assign   │
              │ Extract  │                       │ to Realm │
              └──────────┘                       └──────────┘
```

### Chunking Strategy

Each `.md` file is chunked by `##` headings (H2 sections). Each section becomes a memory with:

- **Content**: The raw markdown text of that section
- **Heading**: The H2 title
- **Parent context**: The H1 title and file path (stored as metadata for context)
- **Tags**: Auto-extracted from content keywords + explicit frontmatter tags

If a section is very large (>2000 tokens), it's further split at `###` (H3) boundaries.

If a file has no headings, the entire file is one memory.

**Configuration per watch directory:**

```toml
[[watch]]
path = "/home/user/projects/myproject/docs"
chunk_by = "section"       # "section" | "paragraph" | "file"
poll_interval = "5m"       # how often to check for changes
realm_hint = "myproject"   # optional: suggest a realm name
```

### Skill-as-Memory Pattern

Each **skill** (from any platform) is ingested as a memory with:
- `chunk_type: "skill"`
- `tags: ["skill", "<platform>", "<category>"]`
- During slumber, skills are **summarized** and **linked** to similar skills via the knowledge graph

---

## Realm System

Realms are auto-discovered clusters of semantically similar memories.

### Auto-Discovery

1. **Initial clustering**: When memories are ingested, they're assigned to the nearest realm by cosine similarity (threshold: 0.75). If no realm is close enough, a new realm is created.
2. **Realm centroids**: Each realm has a centroid vector (mean of all member vectors). This is updated during slumber.
3. **Realm splitting**: During slumber, if a realm grows beyond a configurable size (default: 100 memories), it's evaluated for splitting using k-means (k=2). If the two sub-clusters have sufficient distance, the realm splits.
4. **Realm merging**: If two realm centroids drift close together (< 0.3 cosine distance) during slumber, they merge.

### User-Driven Realms

Users can **pin** a topic as a realm:

```bash
memex8 realm create --name "ex8-terminal" --description "Everything about the EX8 terminal emulator"
```

This creates a realm with a description that gets embedded as the initial centroid. Any memory matching this realm's semantics gets pulled in during the next slumber cycle.

Users can also **upvote** memories (via CLI or Web UI) which increases their `importance` score, making them anchor points in their realm.

### Realm Hierarchy

```
memex8 (root)
├── rust-programming        (auto-discovered)
│   ├── async-patterns      (auto-split from rust-programming)
│   └── error-handling      (auto-split from rust-programming)
├── ex8-terminal            (user-pinned)
│   ├── tauri-integration   (auto-discovered)
│   └── command-blocks      (auto-discovered)
├── devops                  (auto-discovered)
│   └── docker              (auto-discovered)
├── research-papers         (auto-discovered)
│   └── quantization        (auto-discovered)
└── personal-notes          (user-pinned)
```

---

## Slumber Engine

Slumber is the background maintenance process that keeps memories organized and compressed.

### Triggers

| Phase | Trigger | Action |
|-------|---------|--------|
| **Ingest** | Cron schedule (configurable, default: every 5 min) | Poll watched directories, ingest new/changed files |
| **Reorganize** | Idle detection (no API queries for 10 min) | Full slumber pipeline below |

### Slumber Pipeline

```
                    ┌─────────────────────────────────────┐
                    │         SLUMBER PIPELINE             │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  1. INGESTION                        │
                    │  Poll watched dirs, ingest changes   │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  2. DEDUPLICATION                    │
                    │  Find near-duplicates (cosine > 0.95)│
                    │  Keep higher-importance, merge meta  │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  3. SUMMARIZE & COMPRESS             │
                    │  AAAK-style compression of clusters  │
                    │  Preserve essence, link to originals │
                    │  ⚠️ Never delete originals, only add │
                    │     summary pointers                 │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  4. RE-CLUSTER REALMS                │
                    │  Re-compute realm centroids          │
                    │  Move memories to better-fit realms  │
                    │  Split large realms (k-means, k=2)   │
                    │  Merge close realms (cosine < 0.3)   │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  5. TURBOQUANT COMPRESSION           │
                    │  Re-quantize all vectors             │
                    │  Store in `quantized` collection     │
                    │  Configurable bit-width (2.5-4 bits) │
                    │  Near-zero quality loss per paper    │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  6. PRUNE                            │
                    │  Score memories for retention:       │
                    │    score = importance × recency ×    │
                    │           access_count × upvotes     │
                    │  Flag low-score for review (NOT      │
                    │  auto-delete)                        │
                    │  Archive if score < threshold AND    │
                    │  older than 90 days AND no access    │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │  7. UPDATE KNOWLEDGE GRAPH           │
                    │  Extract entities from new memories  │
                    │  Update relationships & temporal     │
                    │  Write MEMEX8.md to project dirs     │
                    └─────────────────────────────────────┘
```

### Summarization Guardrails

The key risk is losing the **essence** of a memory during summarization. To prevent this:

1. **Never delete originals** — summaries are new memories that reference originals
2. **Essence extraction** — when summarizing a cluster, the LLM (or rule-based system) must preserve:
   - Key decisions and their reasoning
   - Unique insights not found elsewhere
   - Entity names and relationships
   - Actionable facts
3. **Confidence scoring** — each summary gets a `compression_confidence` score. Low-confidence summaries are flagged for human review in the Web UI
4. **Rollback** — originals are kept indefinitely; summaries can be deleted without data loss
5. **Upvote protection** — memories with upvotes are never summarized/merged without explicit user approval

### Prune Guardrails

Pruning is conservative:

1. **Never auto-delete** — low-score memories are **flagged** for review, not removed
2. **Archive, don't delete** — pruned memories go to a `memex8_archive` Qdrant collection
3. **Protection rules:**
   - Never prune memories with upvotes > 0
   - Never prune memories accessed in the last 30 days
   - Never prune user-pinned realm anchors
   - Never prune memories created in the last 7 days
4. **Prune review queue** — Web UI shows flagged memories for user decision (keep/archive/delete)

---

## ScalarQuant Integration

### Application to memex8

memex8 uses **Adaptive Scalar Quantization** — a clean, honest name for what the code actually does. Inspired by [arXiv:2504.19874](https://arxiv.org/abs/2504.19874) (TurboQuant) but implemented differently:

- **Data-oblivious quantization** — no training needed, apply instantly
- **Near-optimal MSE** — within 2.7x of information-theoretic lower bound
- **Quality neutrality at 3.5 bits** — no measurable quality loss
- **Zero indexing time** — no preprocessing required

**How it works:**

```
AdaptiveScalarQuant:
  1. Normalize: x_norm = x / ||x||  (unit vector)
  2. Find per-coordinate min/max over the actual vector values
  3. Uniform quantization within that range → pack bits
  4. Store (min, max, norm) alongside packed indices for reconstruction
```

This is simpler than TurboQuant's random rotation + Lloyd-Max codebook approach,
and empirically produces better results on real OpenAI embeddings (cosine 0.971 vs 0.03).

**How we use it:**

1. During slumber, after re-clustering, all memory vectors are re-quantized using ScalarQuant
2. The `quantized` collection stores compressed vectors for fast ANN search
3. Full-resolution vectors remain in `memories` for exact recall when needed
4. Default bit-width: **3.5 bits per channel** (quality-neutral per the paper)
5. Configurable: users can trade quality for compression

**Approximate search flow:**

```
Query vector
    │
    ▼
Quantize query with same ScalarQuant params
    │
    ▼
Search quantized collection (fast, compact)
    │
    ▼
Return top-K IDs
    │
    ▼
Fetch full-resolution vectors from memories collection
    │
    ▼
Re-rank with exact similarity → return results
```

This gives us **fast search** on compressed vectors with **exact ranking** on full vectors.

---

## CLI Reference

```bash
# Setup
memex8 init                          # Interactive setup (provider, model, dirs)
memex8 config show                   # Show current configuration
memex8 config set <key> <value>      # Update config

# Ingestion
memex8 ingest <path>                 # Ingest a .md file or directory
memex8 ingest --watch <path>         # Start watching a directory (foreground)
memex8 watch add <path> [options]    # Add directory to persistent watch list
memex8 watch list                    # List watched directories
memex8 watch remove <path>           # Stop watching a directory

# Search & Retrieval
memex8 search <query>                # Semantic search across all realms
memex8 search <query> --realm <name> # Search within a specific realm
memex8 search <query> --limit 20     # Return top 20 results
memex8 get <memory-id>               # Get full memory by ID
memex8 recall                        # Get highest-importance memories (wakeup)

# Realms
memex8 realms list                   # List all realms with stats
memex8 realms create --name <n>      # Create a user-pinned realm
memex8 realms show <name>            # Show realm details and top memories
memex8 realms merge <a> <b>          # Merge two realms
memex8 realms split <name>           # Force-split a realm

# Memory Management
memex8 upvote <memory-id>            # Increase importance
memex8 prune                         # Show prune review queue
memex8 archive <memory-id>           # Archive a memory
memex8 delete <memory-id>            # Permanently delete (with confirmation)
memex8 edit <memory-id>              # Open memory for editing ($EDITOR)

# Slumber
memex8 slumber status                # Show slumber state and schedule
memex8 slumber trigger               # Manually trigger slumber pipeline
memex8 slumber pause                 # Pause slumber (during heavy use)
memex8 slumber resume                # Resume slumber

# Server
memex8 serve                         # Start REST API + WebSocket server
memex8 mcp                           # Start MCP server (stdio)
memex8 mcp --transport sse --port 8081  # Start MCP server (SSE/HTTP)

# Integrations
memex8 integration openclaw hooks    # Print OpenClaw hook configuration
memex8 integration hermes mcp        # Print Hermes MCP config
memex8 integration pi extension      # Generate pi.dev extension files

# Utilities
memex8 stats                         # Memory/realm/realm statistics
memex8 export [path]                 # Export all memories as JSON
memex8 import <path>                 # Import memories from JSON
memex8 doctor                        # Diagnose connectivity issues
```

---

## REST API

Base URL: `http://localhost:8080/api/v1`

Auth: Bearer token (user-configured API key in `.env`)

### Memory Endpoints

```
POST   /memories                    # Store a new memory
GET    /memories/:id                # Get memory by ID
PUT    /memories/:id                # Update memory content (re-embeds)
DELETE /memories/:id                # Delete memory
POST   /memories/search             # Semantic search
POST   /memories/ingest             # Ingest a file or directory
GET    /memories/recall             # Get high-importance memories (wakeup)
POST   /memories/:id/upvote         # Upvote a memory
POST   /memories/:id/archive        # Archive a memory
```

### Realm Endpoints

```
GET    /realms                      # List all realms
POST   /realms                      # Create a realm
GET    /realms/:id                  # Get realm details + top memories
GET    /realms/:id/graph            # Get realm's knowledge graph
DELETE /realms/:id                  # Delete a realm (memories reassigned)
```

### System Endpoints

```
GET    /stats                       # System statistics
GET    /slumber/status              # Slumber pipeline status
POST   /slumber/trigger             # Manually trigger slumber
GET    /config                      # Get configuration (redacted secrets)
GET    /health                      # Health check
```

### WebSocket

```
WS     /ws                          # Real-time updates
                                     # Events: memory_ingested, slumber_started,
                                     #          slumber_completed, realm_created,
                                     #          realm_split, realm_merged
```

### Example: Store a Memory

```bash
curl -X POST http://localhost:8080/api/v1/memories \
  -H "Authorization: Bearer $MEMEX8_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "content": "## Async Rust Patterns\n\nUse tokio::spawn for fire-and-forget tasks...",
    "source_file": "/home/user/notes/rust.md",
    "tags": ["rust", "async"],
    "realm_hint": "rust-programming"
  }'
```

### Example: Search

```bash
curl -X POST http://localhost:8080/api/v1/memories/search \
  -H "Authorization: Bearer $MEMEX8_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "how to handle async errors in Rust",
    "limit": 10,
    "realm_filter": "rust-programming"
  }'
```

---

## MCP Server

memex8 exposes an MCP server with the following tools (all prefixed `memex8_`):

### Tools

| Tool | Description | Parameters |
|------|-------------|------------|
| `memex8_search` | Semantic search | `query`, `limit?`, `realm?`, `min_score?` |
| `memex8_store` | Store a memory | `content`, `tags?`, `realm_hint?`, `source?` |
| `memex8_recall` | Get important memories | `limit?`, `realm?` |
| `memex8_get` | Get memory by ID | `id` |
| `memex8_ingest` | Ingest file/directory | `path`, `chunk_by?`, `realm_hint?` |
| `memex8_realms_list` | List all realms | — |
| `memex8_realms_show` | Show realm details | `name` |
| `memex8_upvote` | Increase importance | `id` |
| `memex8_stats` | System statistics | — |
| `memex8_slumber_status` | Slumber state | — |
| `memex8_graph_search` | Graph-based retrieval | `entity`, `relationship?`, `depth?` |

### Hermes Integration

In Hermes config (`~/.hermes/config.yaml`):

```yaml
mcp_servers:
  memex8:
    transport: stdio
    command: memex8
    args: ["mcp"]
    # Or for HTTP:
    # transport: http
    # url: http://localhost:8081/mcp
```

Hermes agents can then call `memex8_search`, `memex8_store`, etc. as native tools.

### OpenClaw Integration

Add to OpenClaw workspace `.pi/hooks/` or via `openclaw config`:

```yaml
# Hook: auto-store important conversations
hooks:
  on_conversation_end:
    - type: webhook
      url: http://localhost:8080/api/v1/memories
      method: POST
      headers:
        Authorization: "Bearer ${MEMEX8_API_KEY}"
      body_template: |
        {
          "content": "{{conversation_summary}}",
          "tags": ["conversation", "{{platform}}"],
          "source": "openclaw"
        }
```

### pi.dev Integration

Generate a pi extension:

```bash
memex8 integration pi extension > ~/.pi/agent/extensions/memex8.ts
```

This creates a pi extension that registers custom tools:

```typescript
// ~/.pi/agent/extensions/memex8.ts
export const tools = {
  memex8_search: {
    description: "Search memex8 memory",
    parameters: { query: { type: "string" }, limit: { type: "number" } },
    execute: async (params) => {
      const resp = await fetch("http://localhost:8080/api/v1/memories/search", {
        method: "POST",
        headers: {
          "Authorization": `Bearer ${process.env.MEMEX8_API_KEY}`,
          "Content-Type": "application/json"
        },
        body: JSON.stringify(params)
      });
      return resp.json();
    }
  },
  memex8_store: { /* ... */ }
};
```

---

## MEMEX8.md — Context Bridge

For each watched project directory, memex8 writes a `MEMEX8.md` file containing the most important memories (top 10-20 by importance + recency). This file is picked up by AI agents as context.

**Example output:**

```markdown
# MEMEX8 — Memory Context
<!-- Auto-generated by memex8. Do not edit. Last updated: 2026-04-13T15:30:00Z -->

## Active Realms
- **rust-programming** (45 memories) — Rust language patterns, async, error handling
- **ex8-terminal** (23 memories) — EX8 terminal emulator project
- **devops** (12 memories) — Docker, CI/CD, server management

## Key Memories

### [rust-programming] Async Error Handling Pattern
> Use `anyhow` for applications, `thiserror` for libraries. Chain errors with `.context()`...

### [ex8-terminal] Tauri v2 WebSocket Architecture
> The frontend communicates with the Rust backend via Tauri commands, not raw WebSockets...

### [devops] Docker Compose Health Check Pattern
> Always add healthchecks to dependent services. Use `depends_on.condition: service_healthy`...

---
*Full memory access: `memex8 search "<query>"` or http://localhost:8080*
```

Configuration:

```toml
[memex8_md]
enabled = true
max_memories = 20                # top memories to include
update_on_slumber = true         # rewrite after each slumber cycle
```

---

## Web UI

### Stack
- **Frontend**: React + TypeScript + Vite
- **3D Graph**: Three.js with react-three-fiber (force-directed graph)
- **Styling**: Tailwind CSS
- **Served by**: memex8 core (embedded static assets) or separate container

### Layout

```
┌─────────────────────────────────────────────────────────────┐
│  memex8                          🔍 Search...    [Admin]    │
├────────┬────────────────────────────────────────────────────┤
│        │                                                     │
│ REALMS │  ┌──────────────────────────────────────────────┐  │
│        │  │  📌 Async Error Handling Pattern              │  │
│ ► All  │  │  realm: rust-programming                      │  │
│ ► rust │  │  ↑ 3  •  ingested 2h ago  •  accessed 12x   │  │
│ ► ex8  │  │  Use `anyhow` for applications, thiserror... │  │
│ ► dev  │  └──────────────────────────────────────────────┘  │
│        │                                                     │
│ PRUNE  │  ┌──────────────────────────────────────────────┐  │
│ QUEUE  │  │  📌 Tauri v2 WebSocket Architecture           │  │
│ (3)    │  │  realm: ex8-terminal                          │  │
│        │  │  ↑ 5  •  ingested 1d ago  •  accessed 28x    │  │
│ GRAPH  │  │  The frontend communicates with the Rust...  │  │
│ VIEW   │  └──────────────────────────────────────────────┘  │
│        │                                                     │
│        │  ┌──────────────────────────────────────────────┐  │
│        │  │  ⚠️ Old Config Pattern                       │  │
│        │  │  realm: devops  •  90d old  •  accessed 0x   │  │
│        │  │  [Keep] [Archive] [Delete]                    │  │
│        │  └──────────────────────────────────────────────┘  │
├────────┴────────────────────────────────────────────────────┤
│  🌐 Graph View  │  📋 Browse  │  ⚙️ Admin  │  💤 Slumber  │
└─────────────────────────────────────────────────────────────┘
```

### Features

1. **Reddit-like Card View**
   - Cards show: title/realm, content preview, upvote count, access count, age
   - Upvote/downvote buttons (increases/decreases importance)
   - Click to expand full memory
   - Edit inline (re-embeds on save)
   - Filter by realm, sort by importance/recency/access

2. **Prune Review Queue**
   - Flagged memories shown with keep/archive/delete buttons
   - Confidence score for why it was flagged
   - Bulk actions

3. **3D Knowledge Graph** (Three.js)
   - Nodes = memories (sized by importance, colored by realm)
   - Edges = semantic similarity (threshold > 0.8) or entity relationships
   - Click a node to see memory content in a side panel
   - Zoom, rotate, filter by realm
   - Real-time layout using force-directed algorithm (d3-force-3d)
   - Inspired by nasdanika maven-graph: hover for connections, click to expand

4. **Admin Dashboard**
   - Total memories, realms, storage size
   - Slumber status (running/idle, last run, next scheduled)
   - Ingestion queue
   - Embedding provider status
   - API usage stats

5. **Realm Management**
   - Create/pin new realms
   - View realm details, member count, top memories
   - Merge/split realms
   - See realm evolution over time (growth chart)

---

## Docker Deployment

### Docker Compose

```yaml
version: "3.8"

services:
  qdrant:
    image: qdrant/qdrant:latest
    ports:
      - "6333:6333"
      - "6334:6334"
    volumes:
      - qdrant_data:/qdrant/storage
    environment:
      QDRANT__SERVICE__GRPC_PORT: 6334
    restart: unless-stopped

  memex8:
    build: .
    ports:
      - "8080:8080"    # REST API
      - "8081:8081"    # MCP SSE endpoint
    volumes:
      - ./config.toml:/etc/memex8/config.toml
      - ./watch_dirs:/watch          # Mount directories to watch
      - memex8_data:/var/lib/memex8  # Internal state, cache
    environment:
      - MEMEX8_API_KEY=${MEMEX8_API_KEY}
      - OPENAI_API_KEY=${OPENAI_API_KEY:-}
      - QDRANT_URL=http://qdrant:6333
      - OLLAMA_URL=${OLLAMA_URL:-http://host.docker.internal:11434}
      - RUST_LOG=info
    depends_on:
      - qdrant
    restart: unless-stopped

  # Optional: local embeddings
  ollama:
    image: ollama/ollama:latest
    ports:
      - "11434:11434"
    volumes:
      - ollama_data:/root/.ollama
    # GPU support (uncomment if you have NVIDIA)
    # deploy:
    #   resources:
    #     reservations:
    #       devices:
    #         - driver: nvidia
    #           count: 1
    #           capabilities: [gpu]
    profiles:
      - local-embeddings
    restart: unless-stopped

volumes:
  qdrant_data:
  memex8_data:
  ollama_data:
```

### Dockerfile

```dockerfile
FROM rust:1.82-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/memex8 /usr/local/bin/memex8
COPY web/dist /usr/share/memex8/web

EXPOSE 8080 8081
ENTRYPOINT ["memex8"]
CMD ["serve"]
```

### Quick Start

```bash
git clone https://github.com/user/memex8.git
cd memex8

# Configure
cp .env.example .env
# Edit .env with your API keys

# Start with local embeddings
docker compose --profile local-embeddings up -d

# Or start with OpenAI embeddings only
docker compose up -d

# Initialize
docker compose exec memex8 memex8 init

# Add directories to watch
docker compose exec memex8 memex8 watch add /watch/my-docs --poll-interval 5m

# Open Web UI
open http://localhost:8080
```

---

## Configuration

### config.toml

```toml
[server]
host = "0.0.0.0"
port = 8080
mcp_port = 8081

[auth]
api_key_env = "MEMEX8_API_KEY"    # env var name for user API key

[embedding]
provider = "ollama"                # "ollama" | "openai"
model = "nomic-embed-text"
dimensions = 768                   # must match model

[embedding.ollama]
url = "http://ollama:11434"

[embedding.openai]
api_key_env = "OPENAI_API_KEY"
model = "text-embedding-3-small"
dimensions = 1536

[qdrant]
url = "http://qdrant:6333"
collection_memories = "memories"
collection_quantized = "quantized"
collection_realms = "realms"

[ingest]
default_chunk_by = "section"       # "section" | "paragraph" | "file"
max_chunk_tokens = 2000
poll_interval = "5m"

[realms]
auto_discover = true
similarity_threshold = 0.75        # min cosine sim to join a realm
split_threshold = 100              # memories before considering split
merge_threshold = 0.3              # max cosine dist between centroids to merge

[slumber]
idle_timeout = "10m"               # no queries before slumber starts
cron_ingest = "*/5 * * * *"        # ingest schedule
quantize_bit_width = 3.5           # ScalarQuant bit-width (2.5-8)
auto_archive_days = 90             # days before eligible for archive
prune_threshold = 0.1              # importance score below which to flag

[slumber.summarize]
enabled = true
max_cluster_size = 20              # memories per summary cluster
preserve_originals = true          # never delete originals
confidence_threshold = 0.8         # below this, flag for human review

[memex8_md]
enabled = true
max_memories = 20
update_on_slumber = true

[web]
enabled = true
theme = "dark"

[[watch]]
path = "/watch/docs"
chunk_by = "section"
poll_interval = "5m"

[[watch]]
path = "/watch/notes"
chunk_by = "paragraph"
poll_interval = "1h"
realm_hint = "personal-notes"
```

---

## Directory Structure

```
memex8/
├── Cargo.toml
├── Cargo.lock
├── Dockerfile
├── docker-compose.yml
├── .env.example
├── config.example.toml
├── ARCHITECTURE.md                  # This file
├── README.md
├── LICENSE
│
├── src/
│   ├── main.rs                      # CLI entry point (clap)
│   ├── lib.rs                       # Library root
│   ├── config.rs                    # TOML config parsing
│   │
│   ├── api/
│   │   ├── mod.rs
│   │   ├── server.rs                # Axum server setup
│   │   ├── routes/
│   │   │   ├── mod.rs
│   │   │   ├── memories.rs          # /api/v1/memories
│   │   │   ├── realms.rs            # /api/v1/realms
│   │   │   ├── search.rs            # /api/v1/memories/search
│   │   │   ├── slumber.rs           # /api/v1/slumber
│   │   │   ├── stats.rs             # /api/v1/stats
│   │   │   └── websocket.rs         # /ws real-time events
│   │   ├── auth.rs                  # Bearer token middleware
│   │   └── error.rs                 # Error types
│   │
│   ├── mcp/
│   │   ├── mod.rs
│   │   ├── server.rs                # MCP protocol implementation
│   │   ├── tools.rs                 # Tool definitions
│   │   └── transport.rs             # stdio + SSE transport
│   │
│   ├── engine/
│   │   ├── mod.rs
│   │   ├── ingester.rs              # File watcher + .md parser
│   │   ├── chunker.rs               # Markdown chunking (section/para/file)
│   │   ├── embedder.rs              # Provider abstraction
│   │   ├── providers/
│   │   │   ├── mod.rs
│   │   │   ├── ollama.rs            # Ollama embedding provider
│   │   │   └── openai.rs            # OpenAI embedding provider
│   │   ├── realms.rs                # Auto-discovery, clustering
│   │   ├── slumber.rs               # Slumber pipeline orchestrator
│   │   ├── quantizer.rs             # ScalarQuant implementation
│   │   ├── compressor.rs            # AAAK-style summarization
│   │   ├── search.rs                # Semantic search engine
│   │   ├── graph.rs                 # Entity extraction + knowledge graph
│   │   └── memex8_md.rs             # MEMEX8.md file writer
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── qdrant.rs                # Qdrant client wrapper
│   │   └── migrations.rs            # Collection setup/updates
│   │
│   ├── integrations/
│   │   ├── mod.rs
│   │   ├── openclaw.rs              # Hook config generation
│   │   ├── hermes.rs                # MCP config output
│   │   └── pi.rs                    # Extension/skill generation
│   │
│   └── web/
│       └── embedded.rs              # Serve static Web UI from binary
│
├── web/                             # Web UI (separate build)
│   ├── package.json
│   ├── vite.config.ts
│   ├── src/
│   │   ├── App.tsx
│   │   ├── components/
│   │   │   ├── MemoryCard.tsx       # Reddit-like card
│   │   │   ├── RealmSidebar.tsx     # Realm navigation
│   │   │   ├── PruneQueue.tsx       # Prune review queue
│   │   │   ├── Graph3D.tsx          # Three.js knowledge graph
│   │   │   ├── AdminDashboard.tsx   # Stats and slumber status
│   │   │   ├── SearchBar.tsx
│   │   │   └── MemoryEditor.tsx
│   │   ├── hooks/
│   │   │   ├── useApi.ts
│   │   │   └── useWebSocket.ts
│   │   └── styles/
│   │       └── tailwind.css
│   └── public/
│
├── tests/
│   ├── integration/
│   │   ├── api_tests.rs
│   │   ├── mcp_tests.rs
│   │   └── slumber_tests.rs
│   └── fixtures/
│       ├── sample.md
│       └── config.toml
│
└── scripts/
    ├── setup-ollama.sh              # Pull nomic-embed model
    └── generate-openapi.sh          # Generate OpenAPI spec from routes
```

---

## Roadmap

### Phase 1: Foundation (Week 1-2)
- [ ] Rust project scaffold (Cargo.toml, directory structure)
- [ ] Config parsing (TOML)
- [ ] Qdrant client (collection setup, CRUD)
- [ ] Ollama embedding provider (nomic-embed-text)
- [ ] OpenAI embedding provider
- [ ] Markdown parser + chunker
- [ ] Basic CLI: `init`, `ingest`, `search`, `stats`
- [ ] Docker Compose: Qdrant + memex8

### Phase 2: Realm Engine (Week 3-4)
- [ ] Auto-discovery clustering
- [ ] Realm CRUD (create, split, merge, list)
- [ ] User-pinned realms
- [ ] Importance scoring (upvotes, access tracking)
- [ ] MEMEX8.md writer
- [ ] REST API: memories + realms endpoints

### Phase 3: Slumber (Week 5-6)
- [ ] Idle detection
- [ ] Cron-based ingestion
- [ ] Deduplication
- [ ] AAAK-style summarization/compression
- [ ] ScalarQuant vector quantization
- [ ] Prune flagging with guardrails
- [ ] Knowledge graph (entity extraction, relationships)

### Phase 4: Integrations (Week 7-8)
- [ ] MCP server (stdio + SSE)
- [ ] OpenClaw webhook hooks
- [ ] Hermes MCP config generation
- [ ] pi.dev extension generator
- [ ] WebSocket real-time events
- [ ] Auth (API key)

### Phase 5: Web UI (Week 9-11)
- [ ] React + Vite scaffold
- [ ] Reddit-like memory card browser
- [ ] Upvote/prune/archive actions
- [ ] Realm sidebar navigation
- [ ] 3D force graph (Three.js)
- [ ] Admin dashboard (stats, slumber status)
- [ ] Memory editor (inline edit + re-embed)

### Phase 6: Polish & Release (Week 12)
- [ ] Documentation (README, getting started guide)
- [ ] Docker Hub image
- [ ] `memex8 init` interactive wizard
- [ ] `memex8 doctor` diagnostics
- [ ] Integration tests
- [ ] Performance benchmarks
- [ ] Release v0.1.0

### Future (v0.2+)
- [ ] Multi-user support (auth, per-user collections)
- [ ] Cloud-hosted option
- [ ] More embedding providers (Cohere, Voyage, local GGUF)
- [ ] Memory sharing between users
- [ ] Plugin system for custom ingestors
- [ ] Mobile-responsive Web UI
- [ ] Binary packages (Homebrew, apt, etc.)
- [ ] Graph RAG (retrieve subgraphs, not just vectors)
- [ ] Temporal memory (track how memories change over time)
