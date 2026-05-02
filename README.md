# memex8 — Human-Like Memory for AI Agents

> **Personal project.** Shared because the ideas are worth discussing — not a product, no support commitments. Fork it, adapt it, use it at your own risk.

A self-hosted memory system that models how **human memory actually works**: memories fade over time, related ideas connect automatically, and scattered fragments consolidate into dense summaries.

```
┌──────────────────────┐     ┌──────────────────────┐     ┌──────────────────────┐
│   AI Agent (Hermes)   │────▶│     memex8 Engine     │────▶│     Qdrant DB        │
│   or any MCP client   │◀────│  decay + associations │◀────│  vector storage      │
└──────────────────────┘     │  consolidation + SL   │     └──────────────────────┘
                             └──────────────────────┘
                                      │
                                      ▼
                             ┌──────────────────────┐
                             │     Web UI Graph      │
                             │  memory visualization │
                             └──────────────────────┘
```

**[⭐ Star on GitHub](https://github.com/Ex8-ca/memex8) · [Source (GitLab)](https://gitlab.chillygeek.com/marcus2004/memex8) · [Report Issue](https://github.com/Ex8-ca/memex8/issues)**

---

<img width="1867" height="756" alt="image" src="https://github.com/user-attachments/assets/b6f58404-7e13-48c2-bbcc-aeee32b6a5c3" />


<img width="1867" height="756" alt="image" src="https://github.com/user-attachments/assets/4e7a2029-e403-4409-ac29-8b03d0227e16" />


<img width="1902" height="366" alt="image" src="https://github.com/user-attachments/assets/9862f523-2172-4ac6-90c2-031da43f2153" />


## Why This Exists

Most AI memory systems treat every stored fact as equally important forever. That's not how memory works. memex8 models three real cognitive behaviors:

1. **You forget things** — untouch memories slowly decay
2. **Related things connect** — semantic associations form automatically
3. **Fragments become summaries** — raw conversations consolidate into clean memories

The result: an AI agent that remembers what matters, forgets what doesn't, and connects related ideas without manual tagging.

---

## Key Features

### 🧠 Memory Evolution (What Makes This Different)

| Feature | What It Does | Configurable |
|---------|-------------|--------------|
| **Memory Decay** | Untouched memories lose importance over time (forgetting curve). Floor at 0.05 — nothing is ever fully deleted | Decay rate: 0.001/day |
| **Semantic Associations** | During nightly "slumber", each memory links to its 5 nearest neighbors by vector similarity | Top-K: 5, Min strength: 0.6 |
| **Spreading Activation** | Recalling a memory bumps its associated memories too (0.005). Like how "Tesla" primes "Skar speakers" | Activation bump: 0.005 |
| **Consolidation** | Raw conversation fragments merge into dense summaries. 98 scattered snippets → 5 clean summaries | Trigger: cron or API |

### 🔧 Core

- **Rust binary** with embedded web UI — single deployable artifact
- **Qdrant** for vector storage and semantic search
- **Auto-discovered realms** — memories self-organize into knowledge clusters
- **File watching** — real-time directory monitoring, auto-reingest on change
- **3D force-directed graph** — interactive visualization of memory associations
- **MCP server** — works with Claude Code, Opencode, and any MCP-compatible agent
- **REST API** — full CRUD + search with auth and pagination

### 🔌 Embedding Providers

| Provider | Model | Dimensions | Notes |
|----------|-------|------------|-------|
| **OpenAI** | `text-embedding-3-small` | 1536 | Fast, accurate, cloud-based |
| **Ollama** | `nomic-embed-text` | 768 | Fully local, zero cost, sovereign |

### 💤 Slumber Consolidation

Nightly "sleep" pipeline (9 phases): deduplicate → compress → re-cluster → rename/merge realms → prune stale → **LLM consolidation** → index optimization → apply decay → build associations.

| Backend | Model | Notes |
|---------|-------|-------|
| **OpenAI** | `gpt-4o-mini` | Default, cheap, no GPU needed |
| **Local** | Any OpenAI-compatible endpoint | Fully private consolidation |

---

## Quick Start

### Prerequisites

- Docker & Docker Compose
- OpenAI API key *(or Ollama for fully local embeddings)*

### 1. Clone

```bash
git clone https://github.com/Ex8-ca/memex8.git
cd memex8
```

### 2. Configure

Add to `~/.hermes/.env`:

```bash
MEMEX8_API_KEY=your-secret-key
MEMEX8_BASE_URL=http://localhost:8080
OPENAI_API_KEY=sk-...          # for OpenAI embeddings
# EMBEDDING_PROVIDER=ollama    # optional: use local embeddings
```

### 3. Run

```bash
docker compose up -d
```

### 4. Verify

```bash
curl http://localhost:8080/health
# {"status":"healthy"}
```

### 5. Open the Web UI

```
http://localhost:8080
```

The API key is auto-injected from `.env` — no login needed.

**Web UI features:** Cards view, semantic search, interactive 3D graph, realm filtering, memory detail modal with delete.

---

## Hermes Agent Integration

This is the primary use case — memex8 replaces Hermes' built-in memory system entirely.

### Install the Plugin

```bash
mkdir -p ~/.hermes/plugins
cp -r ~/memex8/plugins/memex8 ~/.hermes/plugins/
```

### Configure

Add to `~/.hermes/config.yaml`:

```yaml
memory:
  provider: "memex8"
  memory_enabled: true
```

### What Happens Automatically

| Trigger | Action |
|---------|--------|
| Before each turn | Background recall → injected as context |
| After each turn | Conversation facts stored as memories |
| Session ends | Full conversation summary sent via webhook |
| Trivial messages | Skipped ("ok", "thanks" aren't stored) |

### Available MCP Tools

`memex8_search` · `memex8_remember` · `memex8_recall` · `memex8_realms` · `memex8_forget` · `memex8_get`

---

## CLI Reference

```bash
memex8 init          # Interactive setup wizard
memex8 ingest ./docs # Ingest files or directories
memex8 watch add .   # Persistent file watcher
memex8 search "query" # Semantic search
memex8 recall        # Wakeup context (top memories)
memex8 slumber status # Slumber status
memex8 slumber trigger # Run maintenance now
memex8 doctor        # Diagnose issues
memex8 serve         # Start REST API server
memex8 mcp           # Start MCP server
memex8 stats         # System statistics
```

---

## REST API

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/v1/memories` | Store a new memory |
| `POST` | `/api/v1/memories/search` | Semantic search |
| `GET`  | `/api/v1/memories/recall` | Top memories |
| `GET`  | `/api/v1/memories/{id}` | Get memory by ID |
| `DELETE` | `/api/v1/memories/{id}` | Delete memory |
| `GET`  | `/api/v1/realms` | List realms |
| `POST` | `/api/v1/slumber/trigger` | Trigger slumber |
| `GET`  | `/api/v1/health` | Health check (no auth) |

All endpoints (except `/health`) require `Authorization: Bearer ***`.

---

## Architecture

```
memex8/
├── src/
│   ├── api/          # REST API (Axum)
│   ├── engine/       # Core logic (embedder, chunker, slumber, realms)
│   ├── storage/      # Qdrant integration
│   ├── mcp/          # MCP server (JSON-RPC 2.0)
│   └── web/          # Embedded web UI
├── plugins/memex8/   # Hermes plugin
├── docker-compose.yml
├── Dockerfile        # Multi-stage Rust build
└── config.example.toml
```

### Ingestion Pipeline

```
.md file → Chunker (pulldown-cmark AST) → Embedder (OpenAI/Ollama) →
Realm Assignment (cosine similarity) → Qdrant Store
```

### Slumber Pipeline (9 Phases)

```
Trigger → Deduplicate → Compress → Re-cluster → Rename/Merge →
Prune Stale → LLM Consolidation → Index Optimization → Decay → Associations
```

---

## Troubleshooting

**Container won't start:**
```bash
docker compose logs memex8
```

**Unauthorized errors:**
```bash
curl -H "Authorization: Bearer $MEMEX8_API_KEY" http://localhost:8080/health
```

**No memories showing up:**
```bash
memex8 stats
memex8 search "test"
```

---

## License

MIT

## Contributing

Personal project shared for reference. No guarantees, no SLA, no support. Fork it, adapt it, build on the ideas.

See [TODO.md](TODO.md) and [PLAN.md](PLAN.md) for the roadmap.
