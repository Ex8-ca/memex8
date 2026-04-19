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
- **Cloud first** — OpenAI `text-embedding-3-small` (1536d), fast and accurate
- **Local fallback** — Ollama with `nomic-embed-text` (768d), zero cost, fully private
- **Pluggable** — Trait-based design, add any embedding provider

### Integrations
- **MCP Server** — JSON-RPC 2.0 over stdio, works with any MCP-compatible agent
- **REST API** — Full CRUD + search with authentication, tag filtering, and pagination
- **Hermes Agent** — Native memory provider plugin (replaces built-in MEMORY.md)
- **OpenClaw** — Webhook hooks for auto-ingesting conversation summaries
- **pi.dev** — TypeScript extension for the pi coding agent
- **Claude Code / Opencode** — Add memex8 MCP server for memory-augmented coding

---

## Quick Start: Get memex8 Running

### Prerequisites
- [Docker & Docker Compose](https://docs.docker.com/compose/install/)
- An [OpenAI API key](https://platform.openai.com/api-keys) *(or use Ollama for local embeddings)*

### 1. Clone and configure
```bash
git clone https://github.com/marcus20232023/memex8.git
cd memex8
cp .env.example .env
nano .env  # Set your OPENAI_API_KEY and change MEMEX8_API_KEY
```

> **Security**: Change `MEMEX8_API_KEY` from the default `memex8-dev-key` to a random string.

### 2. Start everything
```bash
docker compose up -d
```

That's it. memex8 + Qdrant are running.

> **Using local embeddings?** Add `--profile local-embeddings` to start Ollama:
> ```bash
> docker compose --profile local-embeddings up -d
> ```

### 3. Verify it's working
```bash
# Check services
docker compose ps

# Health check
curl http://localhost:8080/health
# Expected: {"status":"healthy"}
```

### 4. (Optional) Build the local CLI binary

Requires [Rust](https://rustup.rs/). Used for file watching, ingesting files, and diagnostics.

```bash
cd memex8
cargo build --release
# Binary at: ./target/release/memex8

# Run diagnostics
cargo run --release -- doctor
```

### 5. Open the web UI
```
http://localhost:8080
```

Enter your `MEMEX8_API_KEY` when prompted (it's saved in localStorage).

---

## Hermes Agent Integration

The deepest integration — memex8 replaces Hermes' built-in memory system entirely.

### Step 1: Install the plugin

Copy the plugin to your Hermes plugins directory:

```bash
mkdir -p ~/.hermes/plugins
cp -r ~/memex8/plugins/memex8 ~/.hermes/plugins/
```

Verify it's in place:
```bash
ls ~/.hermes/plugins/memex8/__init__.py
```

### Step 2: Set environment variables

The memex8 plugin needs to know how to reach your memex8 server. Add these to your `~/.hermes/.env` (or create the file if it doesn't exist):

```bash
MEMEX8_API_KEY=your-key-here
MEMEX8_BASE_URL=http://localhost:8080
```

> **Important**: Use the **same `MEMEX8_API_KEY`** that you set in the memex8 `.env` file. If the keys don't match, the plugin won't be able to authenticate with memex8.

### Step 3: Activate the plugin

Edit `~/.hermes/config.yaml`:

```yaml
memory:
  provider: "memex8"
  memory_enabled: true
```

### Step 4: Add session closure to your SOUL.md

Add this to the end of your Hermes `SOUL.md` so the agent knows when and what to save:

```markdown
## Session Closure

When a session produces real outcomes, save a summary to memex8 via
memex8_remember. Include:

- **Topic** — what was worked on
- **Decisions made** — choices agreed upon
- **New facts discovered** — environment info, API quirks, user preferences
- **Code patterns established** — conventions, architecture choices, workflows
- **Problem solutions found** — bugs fixed with how they were resolved
- **Follow-ups needed** — anything left incomplete or to revisit

Format as a single structured entry. Skip trivial sessions — only save
ones where a future agent would genuinely benefit from knowing what
happened without reading the full transcript.

> **session_search** = raw conversation history
> **memex8** = curated takeaways
```

### Step 5: Restart Hermes

New sessions will use memex8 for all memory operations.

### What happens automatically

| Trigger | Action |
|---------|--------|
| Before each turn | Background recall of relevant memories → injected as context |
| After each turn | Conversation facts stored as searchable memories |
| Trivial messages | Skipped — "ok", "thanks", etc. are not stored |
| Session ends | Full conversation summary sent to memex8 via webhook |
| Built-in `memory` tool | Writes are mirrored to memex8 as well |

### Available tools

| Tool | Description |
|------|-------------|
| `memex8_search` | Semantic search across all memories |
| `memex8_remember` | Store a new memory fact |
| `memex8_recall` | Get high-importance memories (wakeup context) |
| `memex8_realms` | List all knowledge realms |
| `memex8_forget` | Delete a memory by ID |
| `memex8_get` | Get a specific memory by ID |

---

## Other Integrations

### MCP Server (stdio)

For any MCP-compatible agent (Claude Code, Opencode, etc.):

```bash
# Start the MCP server
memex8 mcp
# Or via config:
memex8 integration hermes  # outputs config to paste into agent config
```

### REST API

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

Full REST API docs: [see below](#rest-api-reference)

### OpenClaw (Webhooks)

```bash
memex8 integration openclaw
# Copy output → paste into OpenClaw config → restart
```

### pi.dev

```bash
docker compose exec memex8 memex8 integration pi > ~/.pi/agent/extensions/memex8.ts
```

---

## File Watching

memex8 can watch directories for changes and automatically re-ingest modified files. Powered by the `notify` crate, it debounces events at 500ms and uses SHA-256 comparison to skip unchanged files.

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

**Watched directories stay in sync:** edit a file → memex8 detects the change → re-chunks, re-embeds, updates memory — no manual `memex8 ingest` needed.

---

## Configuration

### Environment variables (`.env`)

| Variable | Purpose | Default |
|----------|---------|---------|
| `MEMEX8_API_KEY` | Auth token for REST/MCP | `memex8-dev-key` |
| `OPENAI_API_KEY` | OpenAI embeddings API key | *(empty)* |
| `EMBEDDING_PROVIDER` | `openai` or `ollama` | `openai` |
| `EMBEDDING_MODEL` | Model name | `text-embedding-3-small` |
| `EMBEDDING_DIMENSIONS` | Vector dimension | `1536` |
| `QDRANT_URL` | Qdrant connection | `http://qdrant:6334` |

See [`.env.example`](.env.example) for the full template.

### TOML config (`config.toml`)

See [`config.example.toml`](config.example.toml) for all options. Key settings:

```toml
[embedding]
provider = "openai"          # or "ollama"
model = "text-embedding-3-small"
dimensions = 1536

[embedding.openai]
api_key_env = "OPENAI_API_KEY"

[slumber]
idle_timeout = "10m"
quantize_bit_width = 3.5     # TurboQuant bit-width (2.5-4)
auto_archive_days = 90
```

Watch configs are added automatically via `memex8 watch add`.

---

## CLI Reference

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

---

## REST API Reference

The REST API runs on `http://localhost:8080` by default. All endpoints except `/health` require Bearer token authentication when `MEMEX8_API_KEY` is set.

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

---

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
├── plugins/memex8/          # Hermes memory provider plugin
│   ├── __init__.py          # MemoryProvider ABC implementation
│   ├── plugin.yaml          # Plugin metadata
│   └── README.md            # Plugin setup docs
├── web-dist/                # Web UI static assets (embedded in binary)
├── docker-compose.yml       # Qdrant + memex8 (+ optional Ollama)
├── Dockerfile               # Multi-stage Rust build
├── Cargo.toml
├── config.example.toml
└── .env.example
```

## How It Works

### Ingestion Pipeline
```
.md file → Chunker (pulldown-cmark AST) → Embedder (OpenAI/Ollama) →
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

---

## Troubleshooting

### "Connection refused"
memex8 isn't running:
```bash
cd ~/memex8 && docker compose ps
docker compose logs memex8
```

### "Unauthorized"
Check your API key:
```bash
curl -H "Authorization: Bearer $MEMEX8_API_KEY" \
  http://localhost:8080/health
```

### Plugin not found
```bash
ls ~/.hermes/plugins/memex8/__init__.py
# Should exist. If not:
cp -r /path/to/memex8/plugins/memex8 ~/.hermes/plugins/
```

### Memories not being recalled
```bash
memex8 stats
memex8 search "your query"
```

---

## License

MIT

## Contributing

Contributions welcome! See [TODO.md](TODO.md) and [PLAN.md](PLAN.md) for current roadmap.
