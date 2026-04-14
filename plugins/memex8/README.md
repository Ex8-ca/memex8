# memex8 Plugin for Hermes-Agent

Self-hosted memory for Hermes using [memex8](https://github.com/marcus20232023/memex8) — Qdrant vector storage with TurboQuant compression and auto-discovered knowledge realms.

## Installation

### 1. Install the plugin

Copy the `plugins/memex8/` directory into your Hermes plugins folder:

```bash
# If memex8 is in Docker, the plugin lives inside the container
# Copy it to your Hermes plugins directory:
cp -r /path/to/memex8/plugins/memex8 ~/.hermes/plugins/memex8
```

Or install from GitHub:

```bash
mkdir -p ~/.hermes/plugins
cd ~/.hermes/plugins
git clone https://github.com/marcus20232023/memex8.git
mv memex8/plugins/memex8 .
rm -rf memex8
```

### 2. Configure

Set the environment variable:

```bash
export MEMEX8_API_KEY=your-api-key
export MEMEX8_BASE_URL=http://localhost:8080  # optional, defaults to localhost
```

Or add to `~/.hermes/config.yaml`:

```yaml
memex8:
  api_key: your-api-key
  base_url: http://localhost:8080
```

### 3. Restart Hermes

Hermes will auto-detect the plugin and load it as a memory provider.

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
- Hermes-Agent with plugin support enabled
