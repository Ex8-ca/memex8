# memex8 Plugin for Hermes-Agent

Self-hosted memory for Hermes using [memex8](https://github.com/marcus20232023/memex8) — Qdrant vector storage with TurboQuant compression and auto-discovered knowledge realms.

## Installation

### 1. Copy the plugin to Hermes

The plugin must go in Hermes' `plugins/memory/` directory (inside the Hermes source tree):

```bash
# Find where Hermes is installed
find ~ -name "hermes" -type f 2>/dev/null | head -5
# Or check your clone location
ls ~/hermes-agent/plugins/memory/ 2>/dev/null

# Copy the plugin
cp -r ~/memex8/plugins/memex8 /path/to/hermes-agent/plugins/memory/
```

### 2. Activate in config

Add to `~/.hermes/config.yaml`:

```yaml
memory:
  provider: memex8
```

### 3. Set environment variables

```bash
export MEMEX8_API_KEY=your-api-key
export MEMEX8_BASE_URL=http://localhost:8080  # optional
```

### 4. Restart Hermes

Hermes will discover the plugin and show it in memory provider settings.

## Available Tools

| Tool | Description |
|------|-------------|
| `memex8_search` | Semantic search across all memories |
| `memex8_recall` | Get high-importance memories (session context) |
| `memex8_remember` | Store a fact for future sessions |
| `memex8_forget` | Delete a memory by ID |
| `memex8_realms` | List knowledge realms with counts |

## How It Works

```
Hermes Agent
  │
  ├─→ memex8_remember  → POST /api/v1/memories
  ├─→ memex8_search    → POST /api/v1/memories/search
  ├─→ memex8_recall    → GET  /api/v1/memories/recall
  ├─→ sync_turn        → auto-store conversations
  └─→ queue_prefetch   → preload recall for next turn
        │
        ▼
  memex8 (Docker: localhost:8080)
        │
        ├─→ Embed (OpenAI/Ollama)
        ├─→ Auto-assign realm
        └─→ Store in Qdrant
```

## Prerequisites

- [memex8](https://github.com/marcus20232023/memex8) running in Docker (`docker compose up -d`)
- Hermes-Agent with plugin support
