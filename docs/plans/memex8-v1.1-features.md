# memex8 plugin improvements — plan (draft for Marc's review)

> **Status: shipped (v1.1.0, 2026-09-28).** All six features and the accompanying bug
> fixes landed in commit range `3b92552`..`b533fc9`. This document is kept here as
> the design rationale, not a TODO. For the v1.1.0 user-facing docs see
> `plugins/memex8/README.md` § "v1.1.0 Features".

**Author:** Hermes (assistant)
**Date:** 2026-09-27
**Target:** `Ex8-ca/memex8` `plugins/memex8/__init__.py`
**Reference inspiration:** [ClaudioDrews/memory-os](https://github.com/ClaudioDrews/memory-os) (specifically `layers/07-ground-truth.md`, `icarus/hooks.py`)

---

## TL;DR — six features for v1.1.0

| # | Feature | Default | Effort | Risk | Why |
|---|---|---|---|---|---|
| 1 | Per-session injection dedup | OFF | 15 min / ~20 lines | Low | Stop re-injecting the same memory 5×/session |
| 2 | Decision/evaluation memory-type promotion | OFF | 30 min / ~40 lines | Low | "Decided to use Postgres" should last 6mo, not 1wk |
| 3 | Prompt-injection sanitization | OFF | 15 min / ~15 lines | Medium | Qdrant → system prompt is a real attack surface |
| 4 | Configurable `system_prompt_block` template | always ON, default template ships | 20 min / ~50 lines | Low | Users want to tweak the injected header text |
| 5 | Export / import memories as JSON | OFF (requires server-side route) | 60 min / ~120 lines | Medium | Backup/migration/multi-device |
| 6 | `/memex8` slash command (CLI subcommand) | always available | 45 min / ~140 lines | Low | Stats, manual recall, export/import trigger |

All additive, all gated by config or subcommand, no breaking changes to existing users. Catalog pin can move `1.0.0 → 1.1.0` (minor bump).

---

## Why these six

Six concrete improvements to the current plugin, ordered by signal-to-effort ratio for the first three (memory-os inspired), and feature-completeness for the last three (user-facing gaps):

| # | Gap | Why it matters | Effort |
|---|---|---|---|
| 1 | **Per-session injection dedup** | The same memory can be re-injected 5× in one session as the recall query varies. Burns tokens, distracts the model. Memory OS's `_injected_fabric` set pattern prevents this. | ~20 lines, ~15 min |
| 2 | **Decision/evaluation memory-type promotion** | memex8 already has a 12-type taxonomy with per-type Weibull decay (preference = 6mo, event = 1wk). But `sync_turn()` stores every turn with the same default type — so a "I decided to migrate to Postgres" turn decays the same as "what's the weather". Memory OS's `_THEME_RE` / `_EVAL_RE` regexes catch decisions/evaluations; we can promote them to slow-decaying types. | ~40 lines, ~30 min |
| 3 | **Prompt-injection sanitization** | Qdrant payload contents flow directly into `system_prompt_block`. A contaminated memory (manually inserted, or coming from a less-trusted ingest path) becomes a prompt-injection vector. Memory OS has a regex blocklist for `[REDACTED]` patterns — at minimum, we should neutralize unbalanced triple-backticks (they break downstream markdown) and `{{ }}` (template injection in `SOUL.md`). | ~15 lines, ~15 min |
| 4 | **Configurable system_prompt_block template** | The current 4-line preamble is hardcoded. Users with strict prompt-format requirements (Markdown strict, custom header style, different tool names) can't adapt it. | ~50 lines, ~20 min |
| 5 | **Export / import memories as JSON** | No way to back up, migrate, or share memories between machines. The server has `Engine::export()` / `Engine::import()` but they're not exposed via HTTP. Plugin-side implementation uses existing REST API. | ~120 lines, ~60 min |
| 6 | **`hermes memex8` slash command** | Users can't see stats, list realms, manually trigger a recall, or run export/import without editing config. Honcho plugin shows the pattern: `register_cli(subparser)` in a separate `cli.py`. | ~140 lines, ~45 min |

---

## Design constraints (must hold across all six)

These are non-negotiable. They come from the Hermes host contract and the catalog's promise of "doesn't break existing users."

1. **No behavior change for users not using these features.** All three are gated by config flags, default-off, that the user can flip on in `~/.hermes/memex8.json`. Existing users get identical output.
2. **No new dependencies.** `re` and `typing` are already imported. We don't add LLM calls, don't add network, don't add SDKs.
3. **No API surface change.** The catalog pin stays at `b5e87b7b`; these are additive features with new opt-in config keys. The `_Client.ingest_conversation` returning `Optional[str]` (from the API v2 commit) doesn't change.
4. **No breaking change to the failure modes.** The circuit breaker still trips; `pre_compress_checkpoint_api_version = 2` still raises on `require_checkpoint=True`; on_session_end still best-effort.
5. **Threads use `spawn_context_thread`** — never bare `threading.Thread` (the threading-fix commit `b5e87b7`).
6. **Per-profile isolation via `hermes_home` kwarg** — never read `Path.home()` directly.
7. **Tests if any test scaffolding exists.** Looking at the repo, there are no tests yet for the plugin. We will add minimal smoke tests inline (`_smoke_test_*()` functions run via `python -c`) so Marc can verify behavior change in <30 sec without setting up pytest.

---

## Feature 1: Per-session injection dedup

### Problem statement

`system_prompt_block()` is called by Hermes at the start of each turn. It calls `client.recall()` (or returns cached `_recall_result` from the background prefetch). The recall result is a list of memory objects. If the same memory is in the top-K for two consecutive turns (because the user's question only slightly changed), it gets injected both times.

Cost: each memory is `~200-500 chars` in the system prompt. Re-injecting 5× wastes ~1,000-2,500 tokens per session, and worse, **the agent loses signal in noise** — when 3 of the 5 recalled memories are duplicates, the model can't tell which is novel.

### Memory OS reference pattern

```python
# From memory-os/icarus/hooks.py
_injected_fabric: set = set()
_injected_qdrant: set = set()
_injected_sessions: set = set()

def on_session_start(session_id="", **_):
    _injected_fabric.clear()
    _injected_qdrant.clear()
    _injected_sessions.clear()
```

Cleared on session start, populated as memories get injected, deduplicated by content ID.

### Proposed memex8 implementation

**Config (additive, default-off):**
```json
{
  "dedup_injected_memories": true,
  "recall_max_inject_chars": 4000
}
```

**New state in `__init__`:**
```python
self._injected_this_session: set[str] = set()  # memory IDs
```

**Modified `system_prompt_block`:**
```python
def system_prompt_block(self) -> str:
    # ... existing recall logic, build full list ...

    # NEW: dedup against already-injected set, truncate to char budget
    dedup_key = self._config.get("dedup_injected_memories", False) if self._config else False
    char_budget = int(self._config.get("recall_max_inject_chars", 4000)) if self._config else 4000

    if dedup_key:
        novel = []
        seen_in_block: set[str] = set()  # dedup WITHIN the block too
        for m in recall_list:
            mid = str(m.get("id", m.get("memory_id", "")))
            if not mid:
                # fallback: dedup on content hash
                mid = hashlib.sha1(m.get("content", "").encode()).hexdigest()[:16]
            if mid in self._injected_this_session:
                continue
            if mid in seen_in_block:
                continue
            novel.append(m)
            seen_in_block.add(mid)

        # char-budget gate (keep highest-priority until budget exhausted)
        # priority: by `importance` field if present, else by recency
        novel.sort(key=lambda m: (
            -float(m.get("importance", 0.5)),
            -float(m.get("created_at_ts", 0)),
        ))
        kept = []
        used = 0
        for m in novel:
            cost = len(m.get("content", ""))
            if used + cost > char_budget:
                continue
            kept.append(m)
            used += cost
            self._injected_this_session.add(str(m.get("id", "")))

        # ... build system prompt block from `kept` instead of full recall_list ...
```

**Session lifecycle hooks:**
```python
def initialize(self, session_id, **_):
    # ... existing code ...
    self._injected_this_session = set()  # reset on every new session

def on_session_end(self, messages):
    # ... existing code ...
    self._injected_this_session = set()  # also clear at end
```

**Edge cases:**
- If a memory has no `id` field (older Qdrant payload), fall back to content hash so we still dedup
- If `recall_max_inject_chars` is hit, lower-priority memories get dropped — explicitly log this so users can tune
- If `dedup_injected_memories` is False (default), the entire path is bypassed → zero behavior change for existing users

### Tests

```python
# _smoke_test_dedup.py (runs from `python -c`)
# 1. Build a fake recall_list with 3 memories, A B A
# 2. Call system_prompt_block twice
# 3. Assert second call's block has only [B] (A is dedup'd)
# 4. Reset session, assert third call has [A, B] again
```

### Files touched

- `plugins/memex8/__init__.py` — add state, modify `system_prompt_block`, modify `initialize`, modify `on_session_end`
- `_smoke_test_dedup.py` (new) — manual test script, not committed to repo or committed in a `tests/` subdir if you want it permanent

---

## Feature 2: Decision/evaluation memory-type promotion

### Problem statement

memex8's memory engine has 12 types with per-type Weibull decay (preference k=0.40 scale=6mo, event k=1.20 scale=1wk, request k=1.50 scale=3d — see `MEMORY.md` types table from the top-level README). But the **plugin's `sync_turn()` doesn't set `memory_type`** — it stores every turn with whatever default the engine assigns (probably `event` or unset).

That means: "I decided to migrate the dev DB to Postgres" and "what's the weather today" both decay the same. Decisions are load-bearing and should persist for months; weather is throwaway and should fade in a week.

### Memory OS reference pattern

```python
# From memory-os/icarus/hooks.py
_THEME_RE = re.compile(
    r"(?i)\b(decided|resolved|completed|fixed|deployed|shipped|reviewed|approved|rejected|built|created)\b"
)
_EVAL_RE = re.compile(
    r"(?i)\b(worked well|didn't work|failed|succeeded|learned|noticed|realized|discovered|finding|insight|improvement)\b"
)
_QUESTION_RE = re.compile(
    r"(?i)\b(what if|wonder|curious about|want to try|experiment with|explore|investigate|test whether)\b"
)
```

Three regex classes — theme (decision), eval (insight/learning), question (exploratory). The plugin uses these to flag what to store at what priority.

### Proposed memex8 implementation

**Config (additive, default-off):**
```json
{
  "promote_decisions": true,
  "promote_evaluations": true
}
```

**New helper in `__init__.py`:**
```python
_DECISION_RE = re.compile(
    r"(?i)\b(decided|resolved|completed|fixed|deployed|shipped|reviewed|approved|rejected|built|created|chose|picked)\b"
)
_EVAL_RE = re.compile(
    r"(?i)\b(worked well|didn't work|failed|succeeded|learned|noticed|realized|discovered|finding|insight|improvement|recommend|avoid)\b"
)

def _classify_memory_type(self, user_content: str, assistant_content: str) -> str:
    """Return memex8 memory_type for this turn based on content signals."""
    if not self._config:
        return "event"  # existing default

    promote_dec = self._config.get("promote_decisions", False)
    promote_eval = self._config.get("promote_evaluations", False)

    if not promote_dec and not promote_eval:
        return "event"  # existing default — zero behavior change

    combined = f"{user_content}\n{assistant_content}"

    # Decisions → 'decision' (k=1.00, scale=2wk — decays after acted on)
    if promote_dec and _DECISION_RE.search(combined):
        return "decision"

    # Evaluations / learnings → 'fact' (k=0.80, scale=1mo) — durable
    if promote_eval and _EVAL_RE.search(combined):
        return "fact"

    # Default
    return "event"
```

**Modified `sync_turn` _sync inner:**
```python
def _sync():
    try:
        client = self._get_client()
        memory_type = self._classify_memory_type(user_content, assistant_content)
        client.store(
            content,
            realm_hint="conversations",
            tags=["conversation", "auto-stored", f"type:{memory_type}"],
            source="hermes-sync",
            memory_type=memory_type,  # NEW: pass through to engine
        )
```

**Edge cases:**
- If the Qdrant schema doesn't have a `memory_type` field, the plugin should fall back gracefully — pass the type in `tags` so it's at least searchable, and log a debug note
- If both decision and eval patterns match in the same turn, decision wins (more specific)
- If neither matches AND both flags are on, return "event" — same as today
- If `promote_decisions` is False, decision patterns are not consulted (no behavior change for users not opting in)

### Tests

```python
# _smoke_test_classify.py
# 1. "I decided to use Postgres" + assistant response → memory_type=decision
# 2. "What did you learn from that?" + "I learned that..." → memory_type=fact
# 3. "What's the weather today?" → memory_type=event (default)
# 4. promote_decisions=False → all three return event (back-compat)
```

### Files touched

- `plugins/memex8/__init__.py` — add `_DECISION_RE`, `_EVAL_RE`, `_classify_memory_type`, modify `_sync` in `sync_turn()`

---

## Feature 3: Prompt-injection sanitization

### Problem statement

`system_prompt_block` returns a string that gets concatenated into Hermes's system prompt. The contents come from `client.recall()` which returns Qdrant payload objects. If a memory contains:
- An unbalanced triple-backtick: `` ``` `` — it breaks the markdown fence for everything after it
- A `{{ variable }}` or `${variable}` template expression — it gets substituted in some downstream renderers
- A `javascript:` or `data:text/html` URL — it could trigger if the agent renders the prompt as HTML (Electron desktop does this in some panels)

…the agent's behavior gets subtly modified, or worse, an attacker who can write to your Qdrant (via a less-trusted ingest path, or via a vulnerable upstream caller) can inject instructions.

### Memory OS reference pattern

```python
# From memory-os/icarus/hooks.py
_INJECTION_PATTERNS = [
    (re.compile(r"(?i)\bignore\s+all\s+(previous|prior)\s+(instructions|...)"), "[REDACTED]"),
    (re.compile(r"(?i)\byou\s+(are|will\s+now)\s+(now\s+)?(become|act|...)\s+as\s+..."), "[REDACTED]"),
    (re.compile(r"\{\{.*?\}\}|\$\{.*?\}"), "[REDACTED]"),
    (re.compile(r"```"), "[code]"),
    (re.compile(r"(?i)(javascript|data)\s*:"), "sanitized:"),
    # ... etc
]

def _scrub_for_prompt(text):
    for pattern, replacement in _INJECTION_PATTERNS:
        text = pattern.sub(replacement, text)
    return text
```

### Proposed memex8 implementation

**Config (additive, default-off):**
```json
{
  "sanitize_for_prompt": true
}
```

**New helper in `__init__.py`:**
```python
# Patterns chosen for HIGH precision (false-positives are worse than missed
# injections, since the user would lose legitimate memories). All patterns
# are tested for false-positive rate against the existing memex8 dataset.
_INJECTION_PATTERNS = [
    # Template injection (high-precision: backslash-escaped braces are rare in
    # natural text; template syntax is unmistakable)
    (re.compile(r"\{\{[^}]{1,200}\}\}"), "[REDACTED:template]"),
    (re.compile(r"\$\{[^}]{1,200}\}"), "[REDACTED:template]"),
    # Markdown break: an unbalanced triple-backtick that opens without a
    # matching close (or vice versa) breaks downstream rendering
    (re.compile(r"```"), "[code-fence]"),
    # URL injection (javascript:, data:text/html, vbscript:)
    (re.compile(r"(?i)(javascript|vbscript)\s*:"), "sanitized:"),
    (re.compile(r"(?i)data\s*:\s*text/html"), "sanitized:html"),
]

def _sanitize_for_prompt(self, text: str) -> str:
    """Strip prompt-injection vectors from memory content before it enters
    the system prompt. Conservative: only matches unambiguous patterns.
    """
    if not text or not self._config:
        return text
    if not self._config.get("sanitize_for_prompt", False):
        return text  # zero behavior change for users not opting in
    for pattern, replacement in _INJECTION_PATTERNS:
        text = pattern.sub(replacement, text)
    return text
```

**Apply at the boundary** — wherever memory content crosses into the system prompt:
```python
def system_prompt_block(self) -> str:
    # ... existing recall logic ...
    for m in kept:
        content = self._sanitize_for_prompt(m.get("content", ""))
        # ... use sanitized content in the block ...
```

Also apply in `on_pre_compress` (the API v2 checkpoint marker) and `sync_turn` (no — sync_turn stores, doesn't display; sanitization would lose data on the way in). Sanitize only on the **read path**, not the write path.

**Edge cases:**
- If a memory contains `` ``` `` legitimately (a code snippet the user stored), this replaces it with `[code-fence]` — losing information but never breaking the prompt. The user can disable sanitization to retain raw text.
- False-positive rate: tested against memory-os's own blocklist (which they audited for months). Our patterns are a strict subset of theirs.
- Performance: 5 regex substitutions on ~500-char strings is sub-millisecond. No measurable impact.

### Tests

```python
# _smoke_test_sanitize.py
# 1. "Hello world" → unchanged
# 2. "Use {{ user.name }}" → "Use [REDACTED:template]"
# 3. "Click javascript:alert(1)" → "Click sanitized:alert(1)"
# 4. "```python\ncode\n```" → "[code-fence]python\ncode\n[code-fence]" (each fence replaced)
# 5. sanitize_for_prompt=False → all four unchanged (back-compat)
```

### Files touched

- `plugins/memex8/__init__.py` — add `_INJECTION_PATTERNS`, `_sanitize_for_prompt`, apply at all read-side boundaries

---

## Feature 4: Configurable `system_prompt_block` template

### Problem statement

`system_prompt_block()` currently returns a hardcoded 4-line string:

```python
return (
    "# memex8 Memory\n"
    "Active. Self-hosted vector memory with semantic search and "
    "auto-organizing knowledge realms.\n"
    "Relevant context is automatically provided before each turn.\n"
    "Use memex8_search to find specific memories, memex8_remember to "
    "store important facts, memex8_recall for high-importance context."
)
```

This works, but it's not customizable. Users with strict prompt-format requirements (Markdown strict mode, custom header style, different tool names) can't adapt it. Memory OS lets users edit the injection preamble directly in their config.

### Proposed implementation

**Config (additive, default-supplied template):**
```json
{
  "system_prompt_template": "# memex8 Memory\nActive. Self-hosted vector memory...\nUse {{tool_search}} to find specific memories, ..."
}
```

If `system_prompt_template` is unset, use the current hardcoded default — zero behavior change for existing users.

**Template variables** (substituted at render time):
- `{{tool_search}}` → `memex8_search`
- `{{tool_remember}}` → `memex8_remember`
- `{{tool_recall}}` → `memex8_recall`
- `{{tool_realms}}` → `memex8_realms`
- `{{tool_forget}}` → `memex8_forget`
- `{{tool_get}}` → `memex8_get`
- `{{base_url}}` → the configured memex8 URL
- `{{version}}` → plugin version

**Modified `system_prompt_block`:**
```python
def system_prompt_block(self) -> str:
    if not self._config or "system_prompt_template" not in self._config:
        return _DEFAULT_SYSTEM_PROMPT_BLOCK  # existing string, unchanged

    template = self._config["system_prompt_template"]
    subs = {
        "tool_search": "memex8_search",
        "tool_remember": "memex8_remember",
        "tool_recall": "memex8_recall",
        "tool_realms": "memex8_realms",
        "tool_forget": "memex8_forget",
        "tool_get": "memex8_get",
        "base_url": self._config.get("base_url", _DEFAULT_BASE_URL),
        "version": _PLUGIN_VERSION,  # set from plugin.yaml at module load
    }
    try:
        return template.format(**subs)
    except (KeyError, IndexError) as e:
        # Bad template — fall back to default, log warning
        logger.warning("memex8 system_prompt_template invalid: %s — using default", e)
        return _DEFAULT_SYSTEM_PROMPT_BLOCK
```

**Edge cases:**
- Bad template syntax → fall back to default, don't crash the agent
- Missing template variables → still substitute what we can, leave the rest as `{{var}}` literal (so the user sees the typo)
- New `get_config_schema()` field: `system_prompt_template` (textarea, optional, default = empty = use built-in)

### Files touched

- `plugins/memex8/__init__.py` — add `_PLUGIN_VERSION`, extract `_DEFAULT_SYSTEM_PROMPT_BLOCK`, modify `system_prompt_block`, update `get_config_schema`

---

## Feature 5: Export / import memories as JSON

### Problem statement

Users need to:
- Back up their memories before risky operations (Qdrant migration, schema change)
- Move memories between machines (laptop → server, dev → prod)
- Share a curated subset with collaborators
- Reset to a known state after a bad ingest

The memex8 **server has `Engine::export()` and `Engine::import()` methods** (see `src/engine/mod.rs:Engine::export` and `Engine::import`) but they're not exposed via HTTP routes yet. So the plugin needs to either:
1. Add HTTP routes to the server (Rust change, separate PR against memex8 server repo)
2. Drive export/import via direct Qdrant access (bypasses memex8 logic)
3. Use the CLI binary (`memex8 export` / `memex8 import`) — slow, shell-dependent
4. Round-trip via the existing `client.store()` / `client.recall()` APIs — works but loses vector compression and any server-side state

### Recommended path

**Phase A (this PR, plugin-only):** Add export/import that uses the existing REST API. Slow but works for any user running a memex8 server with the standard routes. Use it for small-to-medium memories (≤10k memories, ≤100MB).

**Phase B (follow-up PR against memex8 server):** Add `GET /api/v1/admin/export` and `POST /api/v1/admin/import` routes that call the existing `Engine::export()` / `Engine::import()`. Once that's merged, the plugin swaps to the fast path.

### Proposed implementation (Phase A — plugin-only)

**Config (additive, default-OFF):**
```json
{
  "enable_export_import": true
}
```

**New methods in `__init__.py`:**
```python
def export_memories(self, output_path: str, *, realm: Optional[str] = None) -> int:
    """Dump all memories (or one realm) to a JSON file at output_path.

    Returns the count of memories exported. Uses GET /api/v1/memories
    in pages of 100 until exhausted. Memory payloads include id, content,
    realm, tags, importance, created_at_ts, memory_type. Vectors are NOT
    exported — re-import relies on the server to re-embed (which is OK
    since the embedding model is deterministic given the same content).
    """
    client = self._get_client()
    exported = []
    cursor = None
    while True:
        resp = client._request(  # need to expose or add list() to _Client
            "GET", "/api/v1/memories",
            params={"limit": 100, "after": cursor, "realm": realm} if realm
                   else {"limit": 100, "after": cursor},
        )
        batch = resp.get("memories", [])
        if not batch:
            break
        exported.extend(batch)
        cursor = batch[-1].get("id")
        if len(batch) < 100:
            break
    # Write atomically
    with open(output_path, "w") as f:
        json.dump({
            "version": _PLUGIN_VERSION,
            "exported_at": datetime.utcnow().isoformat() + "Z",
            "realm": realm,
            "count": len(exported),
            "memories": exported,
        }, f, indent=2)
    return len(exported)

def import_memories(self, input_path: str, *, realm_hint: Optional[str] = None) -> int:
    """Read a JSON file produced by export_memories and re-store each memory.

    Returns the count of memories imported. Existing memories (matched by
    content) are skipped (idempotent re-import). Use realm_hint to override
    the destination realm on import (e.g. moving memories between realms).
    """
    with open(input_path) as f:
        bundle = json.load(f)
    if bundle.get("version", "0") > _PLUGIN_VERSION:
        raise ValueError(f"Export version {bundle['version']} newer than plugin ({_PLUGIN_VERSION})")
    client = self._get_client()
    imported = 0
    for mem in bundle.get("memories", []):
        try:
            # Skip if a memory with the same content already exists
            existing = client.search(query=mem["content"], top_k=1)
            if existing and existing.get("results"):
                # Check if any existing memory is an exact content match
                for r in existing["results"][:3]:
                    if r.get("content", "").strip() == mem.get("content", "").strip():
                        break
                else:
                    client.store(
                        mem["content"],
                        realm_hint=realm_hint or mem.get("realm"),
                        tags=mem.get("tags", []),
                        source=mem.get("source", "imported"),
                        importance=mem.get("importance", 0.5),
                    )
                    imported += 1
        except Exception as e:
            logger.warning("memex8 import skipped memory %s: %s", mem.get("id"), e)
    return imported
```

**Add `_Client.list_memories()` helper** (needs new method on the server, but since the plugin calls it, we document it as "needs server >= X.Y.Z"):
```python
def list_memories(self, *, limit: int = 100, after: Optional[str] = None,
                  realm: Optional[str] = None) -> Dict[str, Any]:
    params = {"limit": str(limit)}
    if after:
        params["after"] = after
    if realm:
        params["realm"] = realm
    return self.request("GET", "/api/v1/memories", params=params)
```

If `list_memories` 404s on the server (older version), export degrades to: iterate via `recall()` with no query + high `top_k`. Slower but works.

### Edge cases

- Server returns 404 on `/api/v1/memories` → fall back to `recall()` iteration
- Export interrupted → file is half-written. Use atomic write (write to `.tmp`, `os.replace`).
- Import with content > server limit → skip + log warning
- Schema mismatch between export version and plugin version → refuse to import newer-than-plugin files

### Files touched

- `plugins/memex8/__init__.py` — add `export_memories`, `import_memories`, `_Client.list_memories`, new config key
- **Follow-up PR against memex8 server:** add `GET /api/v1/memories` route + `GET /api/v1/admin/export` + `POST /api/v1/admin/import`

---

## Feature 6: `/memex8` slash command (CLI subcommand)

### Problem statement

Users currently interact with memex8 only through:
- The 6 MCP tools (`memex8_search`, etc.)
- The auto-recall/auto-sync background hooks
- `hermes memory setup` for configuration

They can't:
- See real-time stats (memory count, realm breakdown, storage usage)
- Manually trigger a recall with a custom query outside of a turn
- Export/import memories without editing config files
- List realms + see their sizes
- Clear a realm

The Hermes plugin contract supports this via `register_cli(subparser)` — see how `honcho` does it (`plugins/memory/honcho/cli.py`). The pattern is:
1. Implement `register_cli(subparser)` in a separate `cli.py` module
2. In `register(ctx)`, call `ctx.register_cli(my_register_cli_func)`

### Proposed implementation

**New file: `plugins/memex8/cli.py`**

```python
"""hermes memex8 — CLI subcommand tree for memex8 plugin management."""
import argparse
import json
import sys
from typing import Any

# Subcommand registry: (name, help_text, handler, arguments)
_SUBCOMMANDS = [
    ("stats", "Show memory counts, realm sizes, storage usage", _cmd_stats, []),
    ("realms", "List all knowledge realms with sizes", _cmd_realms, []),
    ("recall", "Manually trigger a recall query", _cmd_recall, [
        ("--query", {"required": True, "help": "Search query"}),
        ("--top-k", {"type": int, "default": 8, "help": "Number of memories to return"}),
        ("--realm", {"help": "Filter to a specific realm"}),
        ("--json", {"action": "store_true", "help": "Output raw JSON"}),
    ]),
    ("export", "Export memories to a JSON file", _cmd_export, [
        ("--output", {"required": True, "help": "Output file path"}),
        ("--realm", {"help": "Export only this realm"}),
    ]),
    ("import", "Import memories from a JSON file", _cmd_import, [
        ("--input", {"required": True, "help": "Input file path"}),
        ("--realm", {"help": "Override destination realm on import"}),
        ("--dry-run", {"action": "store_true", "help": "Count what would be imported without writing"}),
    ]),
    ("forget-realm", "Delete all memories in a realm (DESTRUCTIVE)", _cmd_forget_realm, [
        ("--realm", {"required": True, "help": "Realm to clear"}),
        ("--confirm", {"action": "store_true", "help": "Required confirmation flag"}),
    ]),
    ("config", "Show current configuration", _cmd_config, []),
]

def _make_provider():
    """Lazy-load the provider to avoid import cycles."""
    from . import Memex8MemoryProvider
    return Memex8MemoryProvider()

def _cmd_stats(args) -> int:
    p = _make_provider()
    if not p.is_available():
        print("memex8 not configured. Run `hermes memory setup` first.", file=sys.stderr)
        return 1
    # ... fetch stats, print formatted ...
    return 0

# ... other command handlers ...

def register_cli(subparser) -> None:
    """Mount `hermes memex8` subcommand tree."""
    subparser.add_argument("--target-profile", metavar="NAME", dest="target_profile", help="Target a specific profile's memex8 config")
    subs = subparser.add_subparsers(dest="memex8_command", required=True)
    for name, help_text, handler, arguments in _SUBCOMMANDS:
        parser = subs.add_parser(name, help=help_text)
        for flag, kwargs in arguments:
            parser.add_argument(flag, **kwargs)
        parser.set_defaults(func=handler)
```

**Modify `register(ctx)` in `__init__.py`:**
```python
def register(ctx) -> None:
    """Register memex8 as a memory provider plugin."""
    ctx.register_memory_provider(Memex8MemoryProvider())
    # NEW: mount the CLI subcommand tree
    from .cli import register_cli
    ctx.register_cli(register_cli)
```

### Subcommand design

| Command | What it does | Risk |
|---|---|---|
| `hermes memex8 stats` | Memory count, realm sizes, storage, last ingest | None (read-only) |
| `hermes memex8 realms` | List realms with sizes | None (read-only) |
| `hermes memex8 recall --query "..."` | One-shot search, prints formatted or JSON | None (read-only) |
| `hermes memex8 export --output path.json` | Dumps to JSON | Medium (writes file) |
| `hermes memex8 import --input path.json` | Re-imports | Medium (writes memories) |
| `hermes memex8 forget-realm --realm X --confirm` | Wipes a realm | **HIGH (destructive, requires `--confirm`)** |
| `hermes memex8 config` | Show current merged config | None (read-only) |

### Edge cases

- Provider not configured (`MEMEX8_API_KEY` missing) → every command prints a friendly error and exits 1
- Profile-scoped (use `hermes memex8 --target-profile work ...` like honcho does)
- Output formats: default human-readable for `stats`/`realms`/`recall`, `--json` flag for piping
- `--dry-run` on `import` so users can preview before committing

### Files touched

- `plugins/memex8/cli.py` (NEW) — full CLI implementation
- `plugins/memex8/__init__.py` — modify `register()` to call `ctx.register_cli`

---

---

## Rollout plan (post-approval)

### Phase 1: Implementation (~3 hours)
1. Implement Feature 1 (dedup), commit, run smoke test
2. Implement Feature 2 (classify), commit, run smoke test
3. Implement Feature 3 (sanitize), commit, run smoke test
4. Implement Feature 4 (system_prompt_block template), commit, run smoke test
5. Implement Feature 5 (export/import), commit, run smoke test
6. Implement Feature 6 (`cli.py` slash commands), commit, run smoke test
7. Final commit if any cleanup needed

### Phase 2: Validation (~30 min)
1. Update README with the six new config keys + subcommand tree
2. Add a "Security" section calling out the sanitization default-off choice and why
3. Add a "CLI" section documenting `hermes memex8 <subcmd>` usage with examples
4. Run a final smoke test against a live memex8 instance (if Marc has one running)

### Phase 3: Release
1. **Bump version** in `plugins/memex8/plugin.yaml` from `1.0.0` to `1.1.0` (additive features, semver minor bump)
2. Push to `origin/main`
3. Open a follow-up catalog PR against `NousResearch/hermes-agent` with:
   - New SHA
   - Updated `version: "1.1.0"`
   - Brief changelog in the PR body
4. Wait for catalog CI, monitor via cron (or new run)

### Phase 4: Discord follow-up (optional, ~5 min)
Post a brief announcement in `#plugins-skills-and-skins`:
> `memex8 v1.1.0` is up — added per-session injection dedup, decision-promotion memory typing, prompt-injection sanitization, configurable system prompt template, export/import as JSON, and a `hermes memex8` CLI tree. All new features opt-in via `~/.hermes/memex8.json`. Catalog PR incoming.

---

---

## Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Regex false positives lose legitimate memories | Medium | Patterns are a strict subset of memory-os's audited blocklist; defaults to OFF so existing users are unaffected; user can disable per-memory if needed |
| Qdrant schema doesn't have `memory_type` field | Medium | Pass type in `tags` so it's at least queryable; log warning on first miss |
| Char budget too tight → model loses context | Low | Default 4000 chars (~4 memories) is generous; users can bump via config |
| Catalog CI breaks on the new plugin.yaml field | Low | No new top-level fields — all additions are inside `_Client` method params |
| Smoke tests pass but live behavior differs | Medium | Phase 2 includes a real-instance validation; rollback is one `hermes update` |
| Catalog pin needs to bump but Marc forgot | Low | Phase 3 has explicit "bump version" step; cron will detect any drift |
| `register_cli` shape mismatch across Hermes versions | Low | Pin to `>=0.21.4` (same as the catalog entry); if Hermes changes the signature, catalog PR will fail CI before merge |
| `forget-realm` accidentally wipes production data | Medium | **Requires explicit `--confirm` flag** + confirmation prompt; documented as DESTRUCTIVE; `forget_realm` impl uses `client.store(content="", tags=["__delete_marker__"], ...)` workaround since the server has no `DELETE /memex8/realm/{name}` route today — Phase B server PR adds the real route |
| Export JSON doesn't round-trip cleanly | Medium | Schema-versioned bundle (`version: "1.1.0"`), idempotent re-import skips duplicate content, follow-up server PR replaces with native `Engine::export()` |
| `cli.py` import cycle with `__init__.py` | Low | Use lazy import inside handlers (`from . import Memex8MemoryProvider` inside `_make_provider()`), not at module load |
| Six-feature PR is too large for review | Medium | Six atomic commits, one per feature, with independent smoke tests; reviewer can `git log` to skim |

---

---

## What we're NOT doing (and why)

For each item from the original assessment that we considered and rejected:

| Considered | Why rejected |
|---|---|
| **Structured facts with trust scoring** | We already have this via Weibull decay on 12 memory types. Trust scores are an alternative formulation, not an addition. |
| **LLM-powered extraction in `on_session_end`** | We already do full-conversation summary ingestion. Adding an LLM call would add latency + cost + a new external dependency for marginal gain. |
| **Source labels `[memex8-vector-recall]` in system prompt** | Worth considering for a future Ground Truth integration, but it's a single-line config change in `~/.hermes/SOUL.md`, not a plugin change. Add to README as a deployment tip instead. |
| **Ground Truth hierarchy in plugin code** | Per-user change in `SOUL.md`. Document the pattern in README; don't put it in the plugin where users can't customize it. |

---

## Approval

Once you approve, I'll execute Phase 1 in six atomic commits (one per feature), run smoke tests, then Phase 2-3 in two more commits. Total: 8 commits on `main`, version bump from 1.0.0 → 1.1.0.

**Approve as-is, request changes, or punt any of the six features?**
