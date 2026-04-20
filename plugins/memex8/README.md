# memex8 Memory Plugin for Hermes Agent

> Persistent vector memory with semantic search, auto-organizing knowledge realms, and ScalarQuant compression.

## Overview

This plugin replaces Hermes' built-in flat-file memory (MEMORY.md / USER.md) with memex8 — a self-hosted vector database that gives your agent deep, semantic recall across all past conversations and ingested documents.

### Why memex8 over the built-in memory?

| Feature | Built-in (MEMORY.md) | memex8 |
|---------|---------------------|--------|
| Capacity | ~2,200 chars (hard limit) | Unlimited (vector store) |
| Search | None (linear scan) | Semantic vector search |
| Organization | Two buckets (memory/user) | Auto-discovered knowledge realms |
| Dedup | None | SHA-256 + vector similarity |
| External data | No | Ingest projects, Obsidian, email |
| Persistence | Single file | Qdrant vector database |

## Prerequisites

1. **memex8 running** — Docker compose or local binary
2. **Qdrant** — Vector database (included in memex8 docker-compose)
3. **Embedding provider** — Ollama (local) or OpenAI (cloud)

## Quick Setup

### 1. Start memex8

```bash
cd ~/memex8
docker compose up -d
# Verify:
curl http://localhost:8080/health
```

### 2. Install the plugin

```bash
# Copy to Hermes bundled plugins (in the hermes-agent source tree):
cp -r ~/memex8/plugins/memex8 /path/to/hermes-agent/plugins/memory/

# Or to user plugins (preferred for dev):
cp -r ~/memex8/plugins/memex8 ~/.hermes/plugins/
```

### 3. Activate in Hermes

```bash
hermes memory setup
# → Select "memex8"
# → Enter memex8 URL (default: http://localhost:8080)
# → Enter API key
```

Or edit `~/.hermes/config.yaml` directly:

```yaml
memory:
  provider: "memex8"
  memory_enabled: true
```

And set environment variables:

```bash
# In ~/.hermes/.env:
MEMEX8_BASE_URL=http://localhost:8080
MEMEX8_API_KEY=your-key-here
```

### 4. Restart Hermes

New sessions will use memex8 for memory.

## Configuration

### Config file: `~/.hermes/memex8.json`

```json
{
  "base_url": "http://localhost:8080",
  "api_key": "your-key",
  "auto_recall": true,
  "auto_sync": true,
  "recall_top_k": 8,
  "recall_min_score": 0.3,
  "timeout": 10.0
}
```

### Environment variables

| Variable | Purpose |
|----------|---------|
| `MEMEX8_BASE_URL` | memex8 REST API URL (default: `http://localhost:8080`) |
| `MEMEX8_API_KEY` | Authentication token (required) |

### Config precedence

1. **Environment variables** — highest priority (overrides everything)
2. **`~/.hermes/memex8.json`** — persistent config from `hermes memory setup`
3. **Hardcoded defaults** — fallback

## MCP Tools Provided

| Tool | Description |
|------|-------------|
| `memex8_search` | Semantic search across all memories |
| `memex8_remember` | Store a new memory fact |
| `memex8_recall` | Get high-importance memories (wakeup context) |
| `memex8_realms` | List all knowledge realms |
| `memex8_forget` | Delete a memory by ID |
| `memex8_get` | Get a specific memory by ID |

## How It Works

```
Hermes Agent
  │
  │  memory provider → memex8 plugin
  │
  ├── initialize()     → health check, create HTTP client
  ├── prefetch()       → return cached background recall results
  ├── queue_prefetch() → launch async recall before next turn
  ├── sync_turn()      → auto-save conversation turns
  ├── memex8_search    → POST /api/v1/memories/search
  ├── memex8_remember  → POST /api/v1/memories
  ├── memex8_recall    → GET  /api/v1/memories/recall
  ├── on_session_end() → POST /api/v1/webhooks/conversation
  └── on_memory_write()→ mirror built-in memory writes
        │
        ▼
  memex8 Engine
  (chunk → embed → realm → Qdrant store)
```

### Automatic behaviors

- **Auto-recall**: Before each turn, relevant memories are fetched in the background and injected as context
- **Auto-sync**: Conversation turns are stored as memories (skips trivial replies like "ok", "thanks")
- **Session-end**: Full conversation summary is sent via webhook at session close
- **Memory mirroring**: When you use Hermes' built-in `memory` tool (add/replace/remove), memex8 stores a copy too
- **Circuit breaker**: After 5 consecutive failures, API calls pause for 2 minutes to avoid hammering a down server

## Troubleshooting

### "memex8 plugin not found"
```bash
# Check bundled plugins
ls /path/to/hermes-agent/plugins/memory/memex8/__init__.py

# Or check user plugins
ls ~/.hermes/plugins/memex8/__init__.py
```

### "Connection refused"
memex8 isn't running:
```bash
cd ~/memex8 && docker compose ps
```

### "Unauthorized"
Check your API key:
```bash
curl -H "Authorization: Bearer $MEMEX8_API_KEY" \
  http://localhost:8080/health
```

### Memories not being recalled
Check memex8 has data:
```bash
memex8 stats
memex8 search "your query"
```
