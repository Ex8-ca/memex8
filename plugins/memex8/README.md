# memex8 Memory Plugin for Hermes Agent

> Persistent vector memory with semantic search, auto-organizing knowledge realms, and TurboQuant 8× compression.

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

### 2. Install the plugin (catalog — recommended)

```bash
hermes plugins install memex8
hermes memory setup       # pick "memex8", enter URL + API key
```

Then restart Hermes. The catalog entry pins the plugin to a known-good SHA, and future `hermes update` keeps the pin fresh.

### Alternative install paths

```bash
# User-level (no catalog pin — for development):
cp -r plugins/memex8 ~/.hermes/plugins/memex8

# From a private fork (for staged rollouts):
hermes plugins install Ex8-ca/memex8 --ref my-branch
```

The plugin is a **standalone third-party plugin** and is intentionally NOT in
the hermes-agent `plugins/memory/` in-tree directory. See
[Hermes plugin policy](https://hermes-agent.nousresearch.com/docs/developer-guide/plugins)
for why.

### 3. Activate in Hermes

If you skipped `hermes memory setup`, edit `~/.hermes/config.yaml` directly:

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
  "timeout": 10.0,

  "// v1.1.0 features (all opt-in):": "",
  "dedup_injected_memories": false,
  "recall_max_inject_chars": 4000,
  "promote_decisions": false,
  "promote_evaluations": false,
  "sanitize_for_prompt": false,
  "system_prompt_template": "",
  "enable_export_import": false
}
```

See [v1.1.0 Features](#v110-features) below for what each of these does.

### Environment variables

| Variable | Purpose |
|----------|---------|
| `MEMEX8_BASE_URL` | memex8 REST API URL (default: `http://localhost:8080`) |
| `MEMEX8_API_KEY` | Authentication token (required) |

### Config precedence

1. **Environment variables** — highest priority (overrides everything)
2. **`~/.hermes/memex8.json`** — persistent config from `hermes memory setup`
3. **Hardcoded defaults** — fallback

## v1.1.0 Features

All v1.1.0 features are **opt-in** — none change behaviour at default config. Enable them in `~/.hermes/memex8.json`.

### Per-session injection dedup

Stop re-injecting the same memory every turn as the recall query varies. Once a memory has appeared in the system prompt earlier this session, it's skipped on subsequent turns until the session resets.

```json
{
  "dedup_injected_memories": true,
  "recall_max_inject_chars": 4000
}
```

`recall_max_inject_chars` is a soft char budget for the injected block (default off; ignored when `dedup_injected_memories` is off). The actual hard cap on items injected per turn is 5 (`recall_top_k` controls how many the server returns, not how many reach the prompt).

### Decision / evaluation memory-type promotion

"Decided to migrate to Postgres" should outlast "what's the weather". When enabled, `sync_turn()` regex-classifies each turn and stores decisions/evaluations as slow-decaying memory types so they decay slower than the default ephemeral type. Decisions and evaluations are separate config keys so you can enable one without the other.

```json
{
  "promote_decisions": true,
  "promote_evaluations": true
}
```

### Prompt-injection sanitization

Qdrant payload contents flow directly into the injected context block. Enable sanitization to neutralize `{{template}}`, `${template}`, unbalanced triple-backticks (which break downstream markdown), and `javascript:` / `vbscript:` / `data:text/html` URLs. Conservative patterns; defaults to off so existing users are unaffected.

```json
{ "sanitize_for_prompt": true }
```

### Configurable `system_prompt_block` template

The 4-line preamble in the injected block is normally hardcoded. Set `system_prompt_template` to override it. Available variables: `{{realm_count}}`, `{{memory_count}}`, `{{version}}`, `{{base_url}}`. Unknown variables are left literal (no silent drops).

```json
{
  "system_prompt_template": "[memex8 — {{memory_count}} memories across {{realm_count}} realms, v{{version}}]\nUse the memex8_search / memex8_recall tools when relevant context lives outside the current transcript."
}
```

### Export / import memories as JSON

Backup, migrate between machines, or share a realm. Gated by `enable_export_import` so writes to disk must be explicitly opted into.

```json
{ "enable_export_import": true }
```

After enabling, use the `/memex8 export` and `/memex8 import` slash commands to write or read a JSON bundle. Paths are confined to the plugin's data directory unless you've also enabled `enable_export_import` for arbitrary paths.

### `/memex8` slash command

Inspect and operate the plugin without editing config:

```
/memex8 stats       # memory count, realm sizes, last ingest
/memex8 realms      # list realms
/memex8 recall --query "..."
/memex8 export --output /path/to/bundle.json   # requires enable_export_import
/memex8 import --input  /path/to/bundle.json   # requires enable_export_import
/memex8 config      # show the merged config (use this to verify your edits landed)
```

Output is human-readable by default; pass `--json` for machine-readable.

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
  ├── initialize()       → health check, create HTTP client
  ├── prefetch()         → return cached background recall results
  ├── queue_prefetch()   → launch async recall before next turn
  ├── sync_turn()        → auto-save conversation turns
  ├── memex8_search      → POST /api/v1/memories/search
  ├── memex8_remember    → POST /api/v1/memories
  ├── memex8_recall      → GET  /api/v1/memories/recall
  ├── on_session_end()   → POST /api/v1/webhooks/conversation
  ├── on_memory_write()  → mirror built-in memory writes
  └── on_pre_compress()  → archive transcript before lossy rewrite (API v2)
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
- **Circuit breaker**: After 5 consecutive failures, API calls pause for 2 minutes to avoid hammering a down server. **The breaker is bypassed during a pre-compress checkpoint** when `require_checkpoint=True` — see below.

## Pre-Compress Checkpoints (Hermes API v2)

This plugin opts into the **fail-closed pre-compress checkpoint** contract
(`pre_compress_checkpoint_api_version = 2`). What that means:

- Before Hermes performs a lossy context rewrite (memory compression), the
  plugin synchronously archives the full transcript to memex8.
- If the operator enables `compression.checkpoint_required: true` in
  `~/.hermes/config.yaml`, an archive failure raises and **blocks the lossy
  rewrite** — the uncompressed transcript is preserved until the store recovers.
- With `require_checkpoint=False` (the default), archive failures are logged
  and compression proceeds. This is the right default for insight-extraction
  providers, but for an archive like memex8 the checkpoint is load-bearing:
  every successful checkpoint gets a referent (`checkpoint: <archive_id>`) that
  the host forwards into the summary prompt so a later recall can cite it.

To enable fail-closed mode, add to `~/.hermes/config.yaml`:

```yaml
compression:
  checkpoint_required: true    # default: false
```

See [Hermes memory-provider docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin#pre-compress-checkpoints-fail-closed)
for the full contract.

### Implementation note

All background work (`queue_prefetch`, `sync_turn`, `on_session_end`,
`initialize` health probe) runs through `agent.memory_provider.spawn_context_thread`
so writes bind to the correct profile under multiplex. Bare `threading.Thread`
runs with empty contextvars and would silently write to the default profile;
the host's wrapper propagates the spawning context so per-profile writes
(config, secrets, logging scope) land where they should.

## Troubleshooting

### "memex8 plugin not found"

If you installed via the catalog, the path is:

```bash
ls ~/.hermes/plugins/memex8/__init__.py
# (the catalog copies into your HERMES_HOME/plugins/)
```

For development installs:

```bash
ls ~/nvme-data/Documents/myprojs/memex8/plugins/memex8/__init__.py
```

To force a reinstall of the catalog pin:

```bash
hermes plugins remove memex8
hermes plugins install memex8
```

### "Connection refused"
memex8 isn't running:
```bash
cd ~/memex8 && docker compose ps
```

### "Unauthorized"
Check your API key:
```bash
curl -H "Authorization: Bearer *api_key*" \
  http://localhost:8080/api/v1/health
```

### Memories not being recalled
Check memex8 has data:
```bash
memex8 stats
memex8 search "your query"
```
