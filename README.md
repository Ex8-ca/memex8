# memex8 — Self-Hosted AI Memory System

> A Rust-based memory palace for AI agents. Ingest your notes, documents, and skills into organized knowledge realms. Powered by Qdrant vector storage, TurboQuant compression, and auto-discovered semantic clusters.

[![Rust](https://img.shields.io/badge/Rust-1.82+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Qdrant](https://img.shields.io/badge/Qdrant-1.17-2296F3.svg)](https://qdrant.tech/)

## Overview

**memex8** gives AI agents (OpenClaw, Hermes, pi.dev, Claude Code, Opencode, or any MCP-compatible agent) persistent, searchable memory. Instead of re-reading thousands of files on every session, agents query memex8 for relevant context — fast, semantic, and self-organizing.

```
┌─────────────────────────────────────────────────────────┐
│                    AI Agent (any MCP)                    │
│   OpenClaw · Hermes · pi.dev · Claude Code · Opencode   │
├─────────────────────────────────────────────────────────┤
│  MCP/REST API                                            │
│  ┌─────────┐ ┌──────────┐ ┌──────────┐ ┌─────────────┐ │
│  │ search  │ │  store   │ │  recall  │ │   ingest    │ │
│  └────┬────┘ └────┬─────┘ └────┬─────┘ └──────┬──────┘ │
├───────┴───────────┴────────────┴──────────────┴────────┤
│                      Engine                             │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │ Embedder │ │ Chunker  │ │ Realms   │ │  Slumber  │  │
│  │Ollama/Open│ │pulldown-c│ │Auto-clus.│ │Maintenan. │  │
│  └────┬─────┘ └──────────┘ └────┬─────┘ └─────┬─────┘  │
├───────┴──────────────────────────┴──────────────┴────────┤
│                    Qdrant Storage                        │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────────┐  │
│  │ memories │  │  realms  │  │ memories_quantized    │  │
│  │ (vector) │  │(centroid)│  │ (TurboQuant vectors)  │  │
│  └──────────┘  └──────────┘  └───────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

## Features

### Core
- **Vector Memory** — Semantic search across all memories using embeddings
- **Auto-Discovered Realms** — Memories self-organize into knowledge clusters via cosine similarity; realms auto-split via k-means when they grow too large
- **TurboQuant Compression** — Near-optimal vector quantization ([arXiv:2504.19874](https://arxiv.org/abs/2504.19874)): 3.5-bit → 7.6x compression with ~0.90 cosine similarity at 768d
- **Slumber Mode** — Background maintenance with cron + idle triggers: deduplicate, compress, re-cluster, split/merge realms, prune stale memories, write MEMEX8.md files
- **Augment, Don't Replace** — Writes `MEMEX8.md` back to project directories for model context pickup

### Embedding Flexibility
- **Local first** — Ollama with `nomic-embed-text` (768d), zero cost, fully private
- **Cloud fallback** — OpenAI `text-embedding-3-small` or `large`
- **Pluggable** — Trait-based design, add any embedding provider

### Integrations
- **MCP Server** — JSON-RPC 2.0 over stdio, works with any MCP-compatible agent
- **REST API** — Full CRUD + search with authentication, tag filtering, and pagination
- **OpenClaw** — Webhook hooks for auto-ingesting conversation summaries and skill outputs
- **Hermes Agent** — MCP server integration with 11 memory tools
- **pi.dev** — TypeScript extension for the pi coding agent
- **Claude Code** — Add memex8 MCP server to Claude Code's MCP config for memory-augmented coding
- **Opencode** — Add memex8 MCP server to Opencode's config for persistent project context

### CLI
```bash
$ memex8 --help
Self-hosted AI memory system with Qdrant and TurboQuant

Commands:
  init           Interactive setup wizard
  config-show    Show current configuration
  ingest         Ingest a .md file or directory
  watch          Add a directory to the persistent watch list
  search         Semantic search across all memories
  get            Get a specific memory by ID
  recall         Get highest-importance memories (wakeup context)
  realms         Manage knowledge realms
  upvote         Upvote a memory (increase importance)
  prune          Show prune review queue
  archive        Archive a memory
  delete         Permanently delete a memory
  edit           Edit a memory in $EDITOR
  slumber        Slumber management
  serve          Start REST API + WebSocket server
  mcp            Start MCP server
  daemon         Start background daemon (cron + idle scheduler)
  integration    Generate integration configuration
  stats          Show system statistics
  export         Export all memories as JSON
  import         Import memories from JSON
  doctor         Diagnose connectivity issues
```

## Quick Start

### Prerequisites
- Rust 1.82+
- Docker & Docker Compose (for Qdrant)

### Build
```bash
git clone https://github.com/marcus20232023/memex8.git
cd memex8
cargo build --release
```

### Setup
```bash
# Copy config files
cp config.example.toml config.toml
cp .env.example .env

# Edit with your settings
nano .env
```

### Run
```bash
# Check connectivity (Qdrant must be running)
./target/release/memex8 doctor

# Ingest a directory of markdown files
./target/release/memex8 ingest ./my-notes/

# Search your memories
./target/release/memex8 search "async Rust patterns"

# Start the REST API server
./target/release/memex8 serve

# Start the MCP server (for Claude Code, Opencode, etc.)
./target/release/memex8 mcp

# Start the background daemon (cron + idle slumber)
./target/release/memex8 daemon
```

### Docker Compose (Recommended)
Everything comes up together — memex8 + Qdrant + optional Ollama:

```bash
cp .env.example .env

# Start everything (memex8 + Qdrant)
docker compose up -d

# Add local embeddings (Ollama)
docker compose --profile local-embeddings up -d

# Check logs
docker compose logs -f memex8
```

No need to install or run Qdrant separately — Docker Compose handles it.

## Docker Compose

```bash
cp .env.example .env
# Add your API keys to .env

# Start memex8 + Qdrant
docker compose up -d

# With local Ollama embeddings
docker compose --profile local-embeddings up -d
```

## Configuration

See [`config.example.toml`](config.example.toml) for all options. Key settings:

```toml
[embedding]
provider = "ollama"           # or "openai"
model = "nomic-embed-text"    # Ollama model
dimensions = 768

[embedding.openai]
api_key_env = "OPENAI_API_KEY"
model = "text-embedding-3-small"

[slumber]
idle_timeout = "10m"
quantize_bit_width = 3.5      # TurboQuant bit-width (2.5-4)
auto_archive_days = 90
```

## Integrations

### Hermes Agent

```bash
memex8 integration hermes
# Output:
# mcp_servers:
#   memex8:
#     transport: stdio
#     command: memex8
#     args: ["mcp"]
```

Available MCP tools:
| Tool | Description |
|------|-------------|
| `memex8_search` | Semantic search across memories |
| `memex8_store` | Store a new memory |
| `memex8_recall` | Get high-importance memories (wakeup context) |
| `memex8_get` | Get memory by ID |
| `memex8_ingest` | Ingest file or directory |
| `memex8_realms_list` | List all knowledge realms |
| `memex8_realms_show` | Show realm details |
| `memex8_upvote` | Increase memory importance |
| `memex8_stats` | System statistics |
| `memex8_slumber_status` | Slumber pipeline status |
| `memex8_graph_search` | Graph-based memory retrieval |

### OpenClaw

```bash
memex8 integration openclaw
# Outputs webhook configuration for on_conversation_end and on_skill_executed hooks
```

### pi.dev

```bash
memex8 integration pi > ~/.pi/agent/extensions/memex8.ts
```

## REST API

The REST API runs on `http://localhost:8080` by default. All endpoints except `/health` are protected by Bearer token authentication when `MEMEX8_API_KEY` is set.

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/v1/memories` | Store a new memory |
| `POST` | `/api/v1/memories/search` | Search with optional tags, pagination |
| `GET`  | `/api/v1/memories/recall` | Get high-importance memories |
| `POST` | `/api/v1/memories/ingest` | Ingest file or directory |
| `GET`  | `/api/v1/memories/tags` | Get tag suggestions |
| `GET`  | `/api/v1/memories/{id}` | Get memory by ID |
| `DELETE` | `/api/v1/memories/{id}` | Delete a memory |
| `POST` | `/api/v1/memories/{id}/upvote` | Upvote a memory |
| `POST` | `/api/v1/memories/{id}/archive` | Archive a memory |
| `GET`  | `/api/v1/realms` | List all realms |
| `POST` | `/api/v1/realms` | Create a realm |
| `GET`  | `/api/v1/realms/{name}` | Show realm details |
| `GET`  | `/api/v1/slumber/status` | Slumber status |
| `POST` | `/api/v1/slumber/trigger` | Trigger slumber |
| `GET`  | `/api/v1/stats` | System statistics |
| `GET`  | `/api/v1/health` | Health check (no auth) |

### Authentication
```bash
curl -H "Authorization: Bearer $MEMEX8_API_KEY" \
  http://localhost:8080/api/v1/memories/search \
  -d '{"query": "async Rust"}'
```

### Search with Tags and Pagination
```bash
curl -H "Authorization: Bearer $MEMEX8_API_KEY" \
  http://localhost:8080/api/v1/memories/search \
  -d '{"query": "Rust", "tags": ["backend"], "offset": 10, "limit": 20}'
```

## Architecture

```
memex8/
├── src/
│   ├── main.rs              # CLI entry point (clap)
│   ├── config.rs            # TOML configuration
│   ├── lib.rs               # Library exports
│   ├── api/                 # REST API (Axum 0.8)
│   │   ├── server.rs        # HTTP server setup + auth
│   │   ├── auth.rs          # Bearer token middleware
│   │   ├── error.rs         # Error handling
│   │   └── routes/          # Route handlers
│   ├── mcp/                 # MCP server (JSON-RPC 2.0)
│   │   ├── server.rs        # Stdio transport + tool dispatch
│   │   └── tools.rs         # Tool definitions (11 tools)
│   ├── engine/              # Core logic
│   │   ├── mod.rs           # Engine orchestrator
│   │   ├── embedder.rs      # Embedding abstraction
│   │   ├── chunker.rs       # AST-based markdown chunking (pulldown-cmark)
│   │   ├── ingester.rs      # File/directory ingestion
│   │   ├── realms.rs        # Realm management
│   │   ├── slumber.rs       # Background maintenance pipeline
│   │   ├── scheduler.rs     # Cron + idle trigger daemon
│   │   ├── quantizer.rs     # TurboQuant compression
│   │   ├── compressor.rs    # AAAK-style summarization
│   │   ├── search.rs        # Search orchestration
│   │   ├── graph.rs         # Knowledge graph
│   │   ├── doctor.rs        # Diagnostics
│   │   ├── memex8_md.rs     # MEMEX8.md write-back
│   │   └── providers/       # Embedding backends
│   │       ├── ollama.rs    # Ollama API client
│   │       └── openai.rs    # OpenAI API client
│   ├── storage/             # Qdrant integration
│   │   ├── mod.rs           # Module exports
│   │   ├── qdrant.rs        # Qdrant client wrapper (full CRUD)
│   │   └── migrations.rs    # Collection setup
│   ├── integrations/        # Integration generators
│   │   ├── openclaw.rs      # OpenClaw webhook config
│   │   ├── hermes.rs        # Hermes MCP config
│   │   └── pi.rs            # pi.dev extension
│   └── web/                 # Web UI (future)
├── docker-compose.yml       # Qdrant + memex8 + optional Ollama
├── Dockerfile               # Multi-stage Rust build
├── Cargo.toml
├── config.example.toml
└── .env.example
```

## How It Works

### Ingestion Pipeline
```
.md file → Chunker (pulldown-cmark AST) → Embedder (Ollama/OpenAI) →
Realm Assignment (cosine similarity vs centroids) → Qdrant Store
```

### Chunker Strategies
- **section** (default) — Split at H2 headings, preserve code blocks/tables
- **h1** — Split at H1 only (larger chunks)
- **h3** — Split at H3 (smaller chunks)
- **paragraph** — Split at paragraph boundaries
- **file** — One chunk per file

### Search Pipeline
```
Query → Embedder → Qdrant Vector Search → Rank by Score → Filter by tags/realm → Paginate → Results
```

### Recall Pipeline
```
All Memories → Score(importance × recency × access_count) → Sort → Top N → Results
```

### Slumber Mode (Background Maintenance)
```
Trigger (idle timeout / cron) →
  1. Deduplicate (hash-based, keep highest importance)
  2. TurboQuant compress vectors → store in quantized collection
  3. Recompute realm centroids from actual memory vectors
  4. Split large realms via k-means (k=2)
  5. Prune flagging (age × importance × access scoring)
  6. Update MEMEX8.md files per directory
```

### TurboQuant Compression

Based on [arXiv:2504.19874](https://arxiv.org/abs/2504.19874): random orthogonal rotation + Lloyd-Max scalar quantization on the induced Beta distribution.

| Bits | Cosine (768d) | MSE | Packed Size | Compression |
|------|---------------|-----|-------------|-------------|
| 2.0 | 0.79 | 0.0005 | 192 B | 14.5x |
| 2.5 | 0.81 | 0.0005 | 288 B | 10.0x |
| 3.0 | 0.81 | 0.0005 | 288 B | 10.0x |
| 3.5 | 0.90 | 0.0003 | 384 B | 7.6x |
| 4.0 | 0.93 | 0.0002 | 384 B | 7.6x |

## License

MIT

## Contributing

Contributions welcome! See [TODO.md](TODO.md) and [PLAN.md](PLAN.md) for current roadmap.
