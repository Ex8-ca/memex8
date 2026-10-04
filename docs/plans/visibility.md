# Memory visibility — design

## Why

memex8 stores everything in one vector collection. Today every memory
is implicitly shareable with anyone who holds the API key. The
Hermes-A2A bridge needs a way to ship some memories to peers without
leaking the rest. This adds a per-memory `visibility` flag plus the
plumbing for the bridge to honor it.

## Decisions (locked)

- **Two visibility levels:** `private` (default) and `public`.
- **Private is the default on every write path.** Nothing leaves
  memex8 unless the caller explicitly opted in. This is a privacy
  footgun trade — the alternative (public by default) silently leaks
  historic data on upgrade, which is unacceptable.
- **Sharing is per-peer via the existing allowlist**
  (`~/.hermes/a2a_bridge/public.yaml`). No new policy file. The
  existing `policy.resolve_share` already encodes the rules; we
  reuse it.
- **Sharing is pull-based.** A peer that wants my public memories
  sends an A2A `memex8_memory_slice` envelope. My side checks the
  meeting record, the per-memory visibility, and the allowlist. A
  single clear failure mode is returned (no info-leak about which
  check failed).
- **Migration is safe-by-construction.** Every pre-visibility memory
  reads as `"private"`, and `ensure_collections()` stamps
  `"private"` on every legacy record on first startup. Idempotent.

## Files changed

| File | Change |
|---|---|
| `src/storage/qdrant.rs` | New `visibility` field on `MemoryPoint`; `normalize_visibility()` helper; `scroll_memories_by_visibility()`; `backfill_visibility()`; visibility keyword index in `ensure_collections()`. |
| `src/api/routes/memories.rs` | `StoreRequest`, `ListParams`, `SearchRequest` gain optional `visibility`. `PATCH /api/v1/memories/{id}` accepts `visibility` (normalized). New `GET /api/v1/memories/public` discovery endpoint. |
| `src/api/server.rs` | Registers `/memories/public` before `/memories/{id}`. |
| `src/api/routes/webhook.rs` | Webhook ingestion stamps `"private"` explicitly. |
| `src/engine/mod.rs` | `Engine::store_memory` and `Engine::list_memories` take `visibility`. All internal call sites updated. |
| `src/engine/session.rs` | Session summaries + extracted items are `"private"`. |
| `src/engine/backup.rs` | Restore preserves original visibility; legacy records default to `"private"`. |
| `src/mcp/server.rs`, `src/mcp/http.rs` | MCP `store_memory` callers can opt in to public via an optional `visibility` arg; default is private. |

## API contract

```
POST /api/v1/memories
  body: { content, tags?, realm_hint?, source?, visibility? }
  visibility: "private" (default) | "public"
  → 201 { id, status }

PATCH /api/v1/memories/{id}
  body: { memory_type?, importance?, visibility? }
  → 200 { id, status }

GET  /api/v1/memories?visibility=public|private
GET  /api/v1/memories/recall?visibility=public
GET  /api/v1/memories/search (body) { ..., visibility? }

GET  /api/v1/memories/public
  → { memories: [{ id, heading, realm_name, content_preview, tags }], total, limit, offset }
```

`/memories/public` truncates content to 280 characters in the preview
so a curious peer can't pull a 50k-token memory through discovery.

## Test coverage

`#[cfg(test)] mod tests` in `src/storage/qdrant.rs`:
- `normalize_visibility_accepts_public` — case + whitespace tolerant.
- `normalize_visibility_defaults_private_on_unknown` — fail-closed on
  every garbage value.
- `memory_round_trip_preserves_visibility` — payload round-trip via
  the same serializer the engine uses.
- `legacy_memory_reads_as_private` — pre-visibility payloads
  default to `"private"` without backfill.

## Companion work

The plugin (`plugins/memex8/__init__.py`) and the Hermes-A2A bridge
(`Ex8-ca/Hermes-A2A/plugins/a2a_bridge/`) get their own PRs off this
base. See:
- `feature/plugin-visibility` in this repo for the Python client.
- `feature/memex8-slice` in `Ex8-ca/Hermes-A2A` for the bridge side.
