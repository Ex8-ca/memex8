# memex8 — Self-Hosted AI Memory System

> A Rust-based memory palace for AI agents. Ingest your notes, documents, and skills into organized knowledge realms. Powered by Qdrant vector storage, TurboQuant compression, auto-discovered semantic clusters, and real-time file watching.

[![Rust](https://img.shields.io/badge/Rust-1.94+-orange.svg)](https://www.rust-lang.org/)
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
│       │     File Watcher ◄────────────────────┘         │
├───────┴─────────────────────────────────────────────────┤
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
- **File Watching** — Real-time directory monitoring with `notify`; debounced at 500ms, SHA-256 dedup, auto-reingest on change, persistent watch configs
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

### Web UI

When `memex8 serve` is running, open **http://localhost:8080** in your browser.

- **Cards view** — Browse memories in a card layout with upvote, importance, and realm info
- **3D Graph view** — Explore memories as a force-directed graph with realm coloring (orbit, zoom, pan)
- **Realms view** — See all knowledge realms with memory counts; click a realm to filter cards
- **Search** — Full semantic search across all memories
- **Upvote** — Raise the profile/importance of any memory with one click

No build step needed — the web UI is embedded in the binary.

### File Watching

memex8 can watch directories for changes and automatically re-ingest modified files. Powered by the `notify` crate, it debounces events at 500ms and uses SHA-256 comparison to skip files that haven't actually changed.

```bash
# Add a directory to watch
memex8 watch add /home/user/projects

# With options
memex8 watch add /home/user/notes --chunk-by file --realm-hint personal

# List active watches
memex8 watch list

# Remove a watch
memex8 watch remove /home/user/notes

# Ingest + watch in one command
memex8 ingest /home/user/docs --watch
```

**How it works:**
1. On add, memex8 scans all `.md` files and records SHA-256 hashes
2. The `notify` crate watches for filesystem events (recursive)
3. Events are debounced at 500ms — rapid saves don't trigger multiple ingests
4. Before re-ingesting, the file is re-hashed; unchanged files are skipped
5. Watch configs persist to `config.toml` automatically
6. In `memex8 daemon` mode, all configured watchers start automatically

**Watched directories stay in sync:** edit a file in your editor → memex8 detects the change → re-chunks, re-embeds, and updates the memory — all without running `memex8 ingest` again.

### CLI
```bash
$ memex8 --help
Self-hosted AI memory system with Qdrant and TurboQuant

Commands:
  init           Interactive setup wizard
  config-show    Show current configuration
  ingest         Ingest a .md file or directory
  watch          Manage persistent file watchers (add, list, remove)
  search         Semantic search across all memories
  get            Get a specific memory by ID
  recall         Get highest-importance memories (wakeup context)
  realms         Manage knowledge realms (list, create, show, merge, split)
  upvote         Upvote a memory (increase importance)
  prune          Show prune review queue
  archive        Archive a memory
  delete         Permanently delete a memory
  edit           Edit a memory in $EDITOR
  slumber        Slumber management (status, trigger, pause, resume)
  serve          Start REST API + WebSocket server
  mcp            Start MCP server (stdio or SSE transport)
  daemon         Start background daemon (cron + idle scheduler + file watchers)
  integration    Generate integration configuration
  stats          Show system statistics
  export         Export all memories as JSON
  import         Import memories from JSON
  doctor         Diagnose connectivity issues
```

## Quick Start

### Prerequisites
- Docker & Docker Compose

### 1. Clone and configure
```bash
git clone https://github.com/marcus20232023/memex8.git
cd memex8
cp .env.example .env
# Edit .env and set your OPENAI_API_KEY
nano .env
```

### 2. Start everything
```bash
docker compose up -d
```

That's it. memex8 + Qdrant are running.

### 3. Open the web UI
```
http://localhost:8080
```

Enter your `MEMEX8_API_KEY` when prompted (it's saved in localStorage).

### 4. (Optional) Install local binary
For stdio MCP with Claude Code/Opencode, or CLI commands:

```bash
cargo build --release
```

### 5. Set up file watching (optional)
```bash
# Add your project directories
memex8 watch add /home/user/projects --realm-hint projects
memex8 watch add /home/user/notes --realm-hint personal

# Or run the daemon for continuous background processing
memex8 daemon
# The daemon runs the slumber scheduler AND starts all configured file watchers
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

# Watch configs are added automatically via `memex8 watch add`
[[watch]]
path = "/home/user/projects"
chunk_by = "section"
poll_interval = "5m"
realm_hint = "projects"
```

## Integrations

All integrations use the MCP protocol — memex8 acts as an MCP server that agents connect to via stdio.

### Available MCP Tools

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

### Integration: Auto-Ingest via Webhooks

Agents auto-push conversation context to memex8 via webhooks — one command to configure.

#### OpenClaw

```bash
memex8 integration openclaw
# Copy the output → paste into OpenClaw config → restart
```

#### Hermes

```bash
memex8 integration hermes
# Copy the output → paste into ~/.hermes/config.yaml → restart
```

#### What Happens

```
Agent finishes conversation
  │
  │  POST /api/v1/webhooks/conversation
  │  { "summary": "...", "source": "hermes" }
  ▼
memex8 Engine
  ├─→ Embed (OpenAI/Ollama)
  ├─→ Auto-assign realm
  └─→ Store in Qdrant
```

#### Manual Webhook Test

```bash
curl -X POST http://localhost:8080/api/v1/webhooks/conversation \
  -H "Authorization: Bearer $MEMEX8_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"summary": "Test conversation", "source": "manual", "platform": "test"}'
```

### Integration: Two Modes

memex8 supports three integration modes:

| Mode | Transport | Best for |
|------|-----------|----------|
| **Hermes Plugin** | Drop-in Python plugin | Native memory provider |
| **Webhooks** | POST to `/api/v1/webhooks/*` | Auto-ingest from agents |
| **REST API** | HTTP calls to `/api/v1/*` | Manual queries, custom tools |

#### Hermes-Agent Plugin (Native)

The cleanest integration — memex8 runs as a native memory provider inside Hermes:

```bash
# Install the plugin
cp -r plugins/memex8 ~/.hermes/plugins/memex8

# Configure (~/.hermes/config.yaml or env vars)
export MEMEX8_API_KEY=your-key
export MEMEX8_BASE_URL=http://localhost:8080

# Restart Hermes — done
```

Hermes gets 5 new tools: `memex8_search`, `memex8_recall`, `memex8_remember`, `memex8_forget`, `memex8_realms`.
Conversations are auto-stored. No webhooks needed.

### Hermes Agent

Hermes uses a YAML-based MCP config (typically `~/.hermes/config.yaml`):

```yaml
mcp_servers:
  memex8:
    transport: stdio
    command: ~/.memex8/bin/memex8
    args:
      - mcp
```

Or use the built-in generator:

```bash
memex8 integration hermes
# Copy the output into ~/.hermes/config.yaml
```

Restart Hermes after adding the config.

**What happens**: Hermes connects to memex8 on startup. During conversations, Hermes calls `memex8_search` to pull relevant context from your knowledge base, or `memex8_store` to save important findings for future sessions.

### OpenClaw (Webhooks)

OpenClaw integrates via webhooks — when conversations end or skills execute, OpenClaw POSTs summaries to memex8's REST API for auto-ingestion:

```bash
memex8 integration openclaw
# Outputs webhook config for your OpenClaw workspace
```

**What happens**: After every conversation, OpenClaw sends a summary to memex8, which ingests it as a new memory. Context builds automatically over time — no manual intervention needed.

### pi.dev (Docker)

When memex8 runs in Docker, pi.dev connects to the REST API — **no local binary needed**.

1. Start memex8: `docker compose up -d`
2. Generate the extension (the container has the binary):

```bash
docker compose exec memex8 memex8 integration pi > ~/.pi/agent/extensions/memex8.ts
```

3. Edit the generated `memex8.ts` and set the base URL:

```typescript
const BASE_URL = "http://localhost:8080";  // Docker exposes port 8080
const API_KEY = process.env.MEMEX8_API_KEY || "";
```

**What happens**: pi.dev loads the extension on startup. All 4 memory tools (`search`, `store`, `recall`, `ingest`) call the Docker container's REST API at `http://localhost:8080`. The coding agent can query memex8 before making architectural decisions — no local binary, no stdio subprocess.

### pi.dev (Local Binary)

If you installed the local binary:

```bash
memex8 integration pi > ~/.pi/agent/extensions/memex8.ts
```

The generated extension defaults to `http://localhost:8080` — works whether memex8 runs in Docker or as a local `memex8 serve` process.

For any agent that doesn't support MCP:

```bash
# Search
curl -H "Authorization: Bearer $MEMEX8_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "how to deploy", "limit": 5}' \
  http://localhost:8080/api/v1/memories/search

# Store
curl -X POST -H "Authorization: Bearer $MEMEX8_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"content": "# Deployment\nUse docker compose..."}' \
  http://localhost:8080/api/v1/memories
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
│   │   ├── watcher.rs       # File watcher (notify crate, SHA-256 dedup)
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
│   └── web/                 # Web UI (cards, 3D graph, search, upvote)
├── web-dist/                # Web UI static assets (embedded in binary)
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

### File Watcher Pipeline
```
Directory watch (notify crate) → File modified event →
500ms debounce → SHA-256 hash compare → (skip if unchanged) →
Re-chunk → Re-embed → Auto-realm → Update in Qdrant
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
