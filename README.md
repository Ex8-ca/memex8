# memex8 — Human-Like Memory for AI Agents

> **Personal project.** Shared because the ideas are worth discussing — not a product, no support commitments. Fork it, adapt it, use it at your own risk.

A self-hosted memory system that models how **human memory actually works**: memories fade over time at *different rates depending on what they are*, queries re-weight themselves based on *what the user is actually asking for*, related ideas connect automatically, and scattered fragments consolidate into dense summaries. Powered by Google's **TurboQuant** algorithm via [**TurboVec**](https://github.com/RyanCodrai/turbovec) for 8x compressed vector storage.

```
┌──────────────────────┐     ┌────────────────────────────┐     ┌──────────────────────┐
│   AI Agent (Hermes)   │────▶│       memex8 Engine         │────▶│     Qdrant DB        │
│   or any MCP client   │◀────│  Weibull decay (per-type)   │◀────│  payload/metadata    │
└──────────────────────┘     │  Query intent weighting     │     └──────────────────────┘
                             │  Associations + slumber     │
                             │  TurboVec (vectors)         │
                             └────────────────────────────┘
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

Most AI memory systems treat every stored fact as equally important forever, and every search query the same way. That's not how memory works. memex8 models five real cognitive behaviors:

1. **You forget things at different rates** — your preferences age slowly, the events of last Tuesday age fast. Per-type Weibull decay reflects this.
2. **Related things connect** — semantic associations form automatically during nightly slumber
3. **Fragments become summaries** — raw conversations consolidate into dense memories
4. **You ask different questions differently** — "what happened last week" wants recency, "what does marc prefer" wants importance. Query intent classification re-weights accordingly.
5. **Memory is plastic** — memories get re-classified, re-weighted, and re-connected over time as the system understands them better

The result: an AI agent that remembers what matters, forgets what doesn't, answers *the question you're actually asking*, and connects related ideas without manual tagging.

---

## Key Features

### 🧠 Memory Evolution (What Makes This Different)

| Feature | What It Does | Source |
|---------|-------------|--------|
| **Per-type Weibull decay** | Untouched memories fade on a memory-type-specific curve. `preference` (k=0.4, scale=6mo), `fact` (k=0.8, ~1mo), `event` (k=1.2, ~1wk), `request` (k=1.5, ~3 days). Floor at 0.05 — nothing is ever fully deleted. Ported from Mnemosyne's WEIBULL_PARAMS table. | `src/engine/decay.rs` |
| **Query intent classification** | Search queries are classified into one of {temporal, factual, preference, procedural, entity, general}. Each intent re-weights the result scoring: "what happened last week" boosts recency, "what does marc prefer" boosts importance, "how do I deploy" boosts vector similarity. Regex-based, no LLM call on the search path. | `src/engine/query_intent.rs` |
| **Memory verification** | Nightly sweep samples memories and rates confidence they're still true. Stale/contradicted memories rank lower in recall but never disappear. | Slumber phase 13 |
| **Semantic associations** | During nightly slumber, each memory links to its 5 nearest neighbors by vector similarity. | Slumber phase 9 |
| **Spreading activation** | Recalling a memory bumps its associated memories too. Like how "Tesla" primes "Skar speakers". | `Engine::recall()` |
| **Consolidation** | Raw conversation fragments merge into dense summaries. 98 scattered snippets → 5 clean summaries. | Slumber phase 7 |

### Memory Type Reference

Every memory carries a `memory_type` field that determines its decay rate. New memories are typed at ingestion; legacy memories can be retroactively classified with `scripts/classify_memory_types.py` (see below).

| Type | Decay shape (k, scale) | Behavior |
|------|----------------------|----------|
| `profile` | (0.30, 1yr) | Long-term stable — user identity, facts about the person |
| `preference` | (0.40, 6mo) | Long-term stable — likes, dislikes, habits |
| `relationship` | (0.35, 1yr) | Long-term stable — people, connections |
| `fact` | (0.80, 1mo) | Medium-term — general knowledge |
| `entity` | (0.50, 6mo) | Medium-term — named things |
| `setup` | (0.60, 3mo) | Medium-term — configs, endpoints, infra |
| `decision` | (1.00, 2wk) | Decays after the decision is acted on |
| `commitment` | (1.00, 10d) | Decays fast — deadlines |
| `event` | (1.20, 1wk) | Fast-decay — things that happened |
| `instruction` | (0.90, 20d) | Decays — how-to knowledge goes stale |
| `error` | (1.10, 2wk) | Medium-fast — bug descriptions |
| `request` | (1.50, 3d) | Fastest — most requests become irrelevant quickly |
| `general` | (1.00, 1wk) | Default for untyped memories |

### 🔧 Core

- **Rust binary** with embedded web UI — single deployable artifact
- **Qdrant** for payload storage and metadata filtering
- **TurboVec** ([RyanCodrai/turbovec](https://github.com/RyanCodrai/turbovec)) — Google's TurboQuant algorithm for compressed vector search. 8x compression, zero training, data-oblivious, faster than FAISS on ARM
- **Auto-discovered realms** — memories self-organize into knowledge clusters
- **File watching** — real-time directory monitoring, auto-reingest on change
- **3D force-directed graph** — interactive visualization of memory associations
- **MCP server** — works with Claude Code, Opencode, and any MCP-compatible agent
- **REST API** — full CRUD + search with auth and pagination, plus PATCH for partial updates
- **Reaction tracking** — upvote/downvote memories to reinforce or suppress importance
- **Semantic associations** — auto-generated links between related memories (FillGap, TemporalNext, Prerequisite, Companion)
- **Knowledge gap detection** — slumber phase identifies underexplored topic areas
- **Proactive inference API** — suggests what to explore based on existing knowledge
- **Session-end extraction** — LLM-powered summary of decisions and follow-ups from conversations

### 🔌 Embedding Providers

| Provider | Model | Dimensions | Notes |
|----------|-------|------------|-------|
| **OpenAI** | `text-embedding-3-small` | 1536 | Fast, accurate, cloud-based |
| **OpenAI-compatible** | Any | Any | MiniMax, Together AI, Groq — configure `base_url` for cost savings |
| **Ollama** | `nomic-embed-text` | 768 | Fully local, zero cost, sovereign |

### 💤 Slumber Consolidation

Nightly "sleep" pipeline (13 phases): deduplicate → **TurboQuant compression** → re-cluster realms → rename/merge realms → prune stale → **LLM consolidation** → index optimization → spreading activation → **associate memories** → **detect gaps** → **session review** → prune empty realms → **memory verification**.

| Backend | Model | Notes |
|---------|-------|-------|
| **OpenAI** | `gpt-4o-mini` | Default, cheap, no GPU needed |
| **Local** | Any OpenAI-compatible endpoint | Fully private consolidation |
| **TurboQuant** | 4-bit compression | 8x vector size reduction, data-oblivious, 0 training |

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
OPENAI_BASE_URL=https://...      # optional: OpenAI-compatible API (MiniMax, Together, Groq)
# EMBEDDING_PROVIDER=ollama    # optional: use local embeddings
```

### 3. Run

```bash
docker compose up -d
```

### 4. Verify

```bash
curl http://localhost:8080/api/v1/health
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

### Add to SOUL.md

The plugin handles the technical integration — but your SOUL.md is what tells Hermes to actually *use* memex8. Without this, Hermes has the plugin installed but no behavioral instruction to check it.

Add this to `~/.hermes/SOUL.md`:

```markdown
## Memory

You have persistent memory across sessions via **memex8**.

**First, read `~/.memex8/memex8.md`** before each session — it contains:
- Daily slumber digests (dates, summaries of what was worked on)
- Top memories by importance
- How to use the memex8 tools

**To save new memories**, use memex8_remember. Include:
- Topic — what was worked on
- Decisions made — choices agreed upon
- New facts discovered — environment info, API quirks, user preferences
- Code patterns established — conventions, architecture choices, workflows
- Problem solutions found — bugs fixed with how they were resolved
- Follow-ups needed — anything left incomplete or to revisit

Format as a single structured entry. Skip trivial sessions.

> **session_search** = raw conversation history  
> **memex8** = curated takeaways
```

> 💡 **Why this matters:** config.yaml enables the capability — SOUL.md directs the behavior. Both are needed for memex8 to work reliably across sessions.

### What Happens Automatically

| Trigger | Action |
|---------|--------|
| Before each turn | Background recall → injected as context |
| After each turn | Conversation facts stored as memories |
| Session ends | Full conversation summary sent via webhook |
| Trivial messages | Skipped ("ok", "thanks" aren't stored) |

### Available MCP Tools

`memex8_search` · `memex8_remember` · `memex8_recall` · `memex8_realms` · `memex8_forget` · `memex8_get`

**How scoring works under the hood:**

- **`memex8_recall`** — scores by `importance × decay(age, memory_type)`. Each memory's Weibull decay is computed from its `memory_type` and `last_accessed`. Preferences age slow, events age fast, etc. Touches bump importance on access.
- **`memex8_search`** — vector similarity first, then re-weighted by the inferred intent of the query. Temporal queries boost recency, preference queries boost importance, procedural queries boost vector similarity. Falls back to balanced weighting when intent is unclear.

Both endpoints run through the same Weibull + intent logic, so the answer to "what did marc decide yesterday" is appropriately different from "what does marc prefer."

---

## Retroactive Memory Type Classification

Every memory carries a `memory_type` field that drives its decay rate. New memories are typed at ingestion via the `ExtractedItem.memory_type` field. To classify existing memories (the ones that shipped before the field existed, currently `general` by default):

```bash
cd ~/memex8

# 1. Preview what would change (no writes)
LLM_API_URL=https://api.openai.com \
LLM_MODEL=gpt-4.1-mini \
OPENAI_API_KEY=sk-... \
python3 scripts/classify_memory_types.py --dry-run

# 2. Apply for real
LLM_API_URL=https://api.openai.com \
LLM_MODEL=gpt-4.1-mini \
OPENAI_API_KEY=sk-... \
python3 scripts/classify_memory_types.py
```

The script runs in two passes:

1. **Keyword classifier** — fast regex match on content for obvious signals (`like/prefer/favorite` → `preference`, ISO dates → `event`, `wife/husband/friend` → `relationship`, `my name/I am/I live` → `profile`, `port/host/url/env var/docker` → `setup`, `error/bug/broken/crash` → `error`, `decided/going with` → `decision`, `how to/step/install` → `instruction`)
2. **LLM fallback** — sends keyword-miss memories to your configured chat model with a small classification prompt. Falls back to `general` if no `LLM_API_URL` is set.

**Confidence handling:** if neither pass is confident, the memory stays `general` (safe default with k=1.0, 1wk decay). **Slumber-summary memories** (`chunk_type=consolidated`) are auto-skipped — they're already abstract combinations of multiple memories.

**Cost:** roughly **$0.01 per 1,000 memories** with `gpt-4.1-mini`. Keyword-only mode is free.

The script writes via the new `PATCH /api/v1/memories/{id}` endpoint, which accepts `{memory_type, importance}` for surgical updates.

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

### Memories & Realms

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET`  | `/api/v1/memories` | List all memories (sort: `ingested_at`, `importance`, `last_accessed`, `access_count`) |
| `POST` | `/api/v1/memories` | Store a new memory |
| `POST` | `/api/v1/memories/search` | Semantic search |
| `GET`  | `/api/v1/memories/recall` | Top memories |
| `GET`  | `/api/v1/memories/verification-summary` | Count memories by verification status (verified/stale/contradicted/unverified) |
| `GET`  | `/api/v1/memories/{id}` | Get memory by ID |
| `PATCH` | `/api/v1/memories/{id}` | Partial payload update — accepts `{memory_type, importance}`. Used by the retroactive classifier script |
| `DELETE` | `/api/v1/memories/{id}` | Delete memory |
| `GET`  | `/api/v1/realms` | List realms |
| `POST` | `/api/v1/slumber/trigger` | Trigger slumber |
| `GET`  | `/api/v1/health` | Health check (no auth) |

### Reactions & Associations

|| Method | Endpoint | Description |
|--------|----------|-------------|
|| `POST` | `/api/v1/memories/{id}/react` | Upvote or downvote a memory |
|| `GET`  | `/api/v1/memories/{id}/associations` | Get semantic links for a memory |

### Proactive Inference

|| Method | Endpoint | Description |
|--------|----------|-------------|
|| `GET`  | `/api/v1/inference/gaps` | List open knowledge gaps |
|| `POST` | `/api/v1/inference/suggest` | Get gap suggestions for a topic |
|| `POST` | `/api/v1/inference/gaps/{id}/resolve` | Mark a gap as resolved |
|| `POST` | `/api/v1/inference/gaps/{id}/dismiss` | Dismiss a gap |

All endpoints (except `/health`) require `Authorization: Bearer <key>`

---

## Architecture

```
memex8/
├── src/
│   ├── api/          # REST API (Axum) — CRUD, search, recall, PATCH
│   ├── engine/       # Core logic
│   │   ├── decay.rs         # Per-type Weibull decay (WEIBULL_PARAMS)
│   │   ├── query_intent.rs  # Regex classifier + intent weight biases
│   │   ├── embedder/        # OpenAI / Ollama / OpenAI-compatible
│   │   ├── chunker/         # Markdown AST → semantic chunks
│   │   ├── slumber/         # 13-phase nightly pipeline
│   │   ├── realms/          # Auto-discovered knowledge clusters
│   │   └── quantizer/       # TurboVec 4-bit compressed search index
│   ├── storage/      # Qdrant integration (payload/metadata only)
│   ├── mcp/          # MCP server (JSON-RPC 2.0)
│   └── web/          # Embedded web UI
├── scripts/          # One-shot maintenance scripts
│   └── classify_memory_types.py  # Retroactive keyword+LLM classifier
├── plugins/memex8/   # Hermes plugin
├── docker-compose.yml
├── Dockerfile        # Multi-stage Rust build
└── config.example.toml
```

### Ingestion Pipeline

```
.md file → Chunker (pulldown-cmark AST) → Embedder (OpenAI/Ollama) →
Realm Assignment (cosine similarity) → Qdrant Store with memory_type →
TurboVec index (on slumber)
```

New memories are stamped with a `memory_type` at ingestion time (default `general`). The decay curve then drives recall ranking automatically — no manual weight tuning per memory.

### Recall Scoring

```
final_score = importance
              × weibull_decay(last_accessed, memory_type)
              × (1 + access_count × 0.05)
              × verification_multiplier
```

`weibull_decay()` is in `src/engine/decay.rs` — a pure function `(timestamp, query_time, memory_type) → f64` in `(0.0, 1.0]`. Defaults to the `general` curve for empty/unknown types.

### Search Scoring

```
raw_score = vector_similarity (cosine, after TurboVec fast-path)
adjusted  = raw_score
            × vector_bias[intent]
            × importance_factor(importance, importance_bias[intent])
            × recency_factor(decay, recency_bias[intent])
```

Intent weights come from `src/engine/query_intent.rs::weights_for(query)` — table of `(vector, importance, recency)` multipliers per intent class. Unclassified queries get the balanced default `(1.0, 1.0, 1.0)`.

### Slumber Pipeline (13 Phases)

```
Trigger → Deduplicate → TurboQuant Compress → Re-cluster → Rename/Merge →
Prune Stale → LLM Consolidation → Index Opt → Spreading → Associate → Gap Detect →
Session Review → Prune Empty Realms → Memory Verification
```

---

## Troubleshooting

**Container won't start:**
```bash
docker compose logs memex8
```

**Unauthorized errors:**
```bash
curl -H "Authorization: Bearer ***" http://localhost:8080/api/v1/health
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
