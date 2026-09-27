"""memex8 memory plugin — MemoryProvider interface for Hermes Agent.

Self-hosted vector memory with semantic search, auto-organizing knowledge
realms, and ScalarQuant compression.

Features:
  - Semantic vector search via Qdrant
  - Auto-recall before each turn (background prefetch)
  - Auto-sync conversation turns to memory
  - Session-end ingestion of full conversation summaries
  - Mirrors built-in memory writes to memex8
  - Circuit breaker for API resilience
  - Configurable via env vars or ~/.hermes/memex8.json

Config resolution:
  1. Environment variables: MEMEX8_BASE_URL, MEMEX8_API_KEY
  2. $HERMES_HOME/memex8.json (profile-scoped)

Environment variables:
  MEMEX8_BASE_URL  — memex8 REST API endpoint (default: http://localhost:8080)
  MEMEX8_API_KEY   — Authentication token (required)
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider, spawn_context_thread
from tools.registry import tool_error

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_DEFAULT_BASE_URL = "http://localhost:8080"
_DEFAULT_RECALL_TOP_K = 8
_DEFAULT_SEARCH_TOP_K = 8
_DEFAULT_RECALL_MIN_SCORE = 0.3
_DEFAULT_TIMEOUT = 10.0
_DEFAULT_SEARCH_TIMEOUT = 15.0
# Plugin version (kept in sync with plugin.yaml) — exposed via
# {{version}} in the configurable system_prompt_block template
_PLUGIN_VERSION = "1.1.0"

# Trivial messages that shouldn't be synced to memory
_TRIVIAL_RE = re.compile(
    r"^(ok|okay|thanks|thank you|got it|sure|yes|no|yep|nope|k|ty|thx|np)\.?$",
    re.IGNORECASE,
)

# Feature 2: memory-type classifiers
# Decision/decision-verb pattern (kept conservative — anchored on whole words,
# excludes the "I decided to ask about X" non-load-bearing sense)
_DECISION_RE = re.compile(
    r"(?i)\b(decided|resolved|completed|fixed|deployed|shipped|reviewed|"
    r"approved|rejected|built|created|chose|picked|migrated|adopted|"
    r"switched|locked in|signed off)\b"
)
# Evaluation/learning pattern (insight that should persist longer than a week)
_EVAL_RE = re.compile(
    r"(?i)\b(worked well|didn't work|didnt work|failed|succeeded|learned|"
    r"noticed|realized|discovered|finding|insight|improvement|recommend|"
    r"recommendation|avoid|conclusion|takeaway)\b"
)

# Feature 3: prompt-injection sanitization patterns
# Patterns chosen for HIGH PRECISION — false positives are worse than missed
# injections because they destroy legitimate memory content. Audited against
# the memos dataset + the memory-os `_INJECTION_PATTERNS` blocklist.
_SANITIZE_PATTERNS = [
    # Template injection (unambiguous: backslash-escaped braces are rare in
    # natural text; template syntax is unmistakable)
    (re.compile(r"\{\{[^{}]{1,200}\}\}"), "[REDACTED:template]"),
    (re.compile(r"\$\{[^{}]{1,200}\}"), "[REDACTED:template]"),
    # Markdown-break: an unbalanced triple-backtick that opens without a
    # matching close (or vice versa) breaks downstream rendering of the
    # system prompt. Conservative — only catches triple-backticks specifically;
    # single-backtick code spans (`foo`) are common in technical memories
    # and are left alone.
    (re.compile(r"```"), "[code-fence]"),
    # URL-scheme injection: javascript:, vbscript:, data:text/html
    (re.compile(r"(?i)(javascript|vbscript)\s*:"), "[REDACTED:url-scheme]:"),
    (re.compile(r"(?i)data\s*:\s*text/html"), "[REDACTED:html-data]:"),
]


def _sanitize_for_prompt(text: str, enabled: bool = True) -> str:
    """Strip prompt-injection vectors from memory content before it enters the
    system prompt. Conservative — only matches unambiguous patterns.

    Feature 3. Set ``enabled=False`` to bypass (default-off for users; opt-in
    via ``sanitize_for_prompt=true`` in config).
    """
    if not text or not enabled:
        return text
    for pattern, replacement in _SANITIZE_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


# Feature 4: default system-prompt preamble. Used as fallback when the user
# hasn't set `system_prompt_template` in memex8.json. Exported as a constant
# so it can be tested independently.
_DEFAULT_SYSTEM_PROMPT_BLOCK = (
    "# memex8 Memory\n"
    "Active. Self-hosted vector memory with semantic search and "
    "auto-organizing knowledge realms.\n"
    "Relevant context is automatically provided before each turn.\n"
    "Use memex8_search to find specific memories, memex8_remember to "
    "store important facts, memex8_recall for high-importance context."
)

# Available {{variables}} for system_prompt_template (Feature 4)
_TEMPLATE_VARS = {
    "tool_search": "memex8_search",
    "tool_remember": "memex8_remember",
    "tool_recall": "memex8_recall",
    "tool_realms": "memex8_realms",
    "tool_forget": "memex8_forget",
    "tool_get": "memex8_get",
    "version": _PLUGIN_VERSION,
}


def _render_system_prompt(template: str, base_url: str) -> str:
    """Substitute {{variables}} in a user-supplied system-prompt template.

    Feature 4. Unknown variables are left as literal ``{{name}}`` so the
    user sees the typo (rather than crashing the agent at turn-start).
    """
    from re import Match
    subs = dict(_TEMPLATE_VARS)
    subs["base_url"] = base_url
    def repl(match: Match[str]) -> str:
        key = match.group(1).strip()
        return subs.get(key, "{{" + key + "}}")
    return re.sub(r"\{\{\s*(\w+)\s*\}\}", repl, template)


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

def _default_config() -> dict:
    return {
        "base_url": _DEFAULT_BASE_URL,
        "api_key": "",
        "auto_recall": True,
        "auto_sync": True,
        "recall_top_k": _DEFAULT_RECALL_TOP_K,
        "search_top_k": _DEFAULT_SEARCH_TOP_K,
        "recall_min_score": _DEFAULT_RECALL_MIN_SCORE,
        "timeout": _DEFAULT_TIMEOUT,
    }


def _load_config(hermes_home: str) -> dict:
    """Load config with proper precedence: env vars > JSON > defaults.

    Resolution order (highest to lowest):
      1. Environment variables (MEMEX8_BASE_URL, MEMEX8_API_KEY)
      2. $HERMES_HOME/memex8.json (persistent config from setup)
      3. Hardcoded defaults
    """
    # Start with defaults
    config = _default_config()

    # Apply JSON file overrides
    config_path = Path(hermes_home) / "memex8.json"
    if config_path.exists():
        try:
            file_cfg = json.loads(config_path.read_text(encoding="utf-8"))
            for key, value in file_cfg.items():
                if value is not None and value != "":
                    config[key] = value
        except Exception as e:
            logger.debug("Failed to load memex8.json: %s", e)

    # Env vars always take final precedence
    if os.environ.get("MEMEX8_BASE_URL"):
        config["base_url"] = os.environ["MEMEX8_BASE_URL"]
    if os.environ.get("MEMEX8_API_KEY"):
        config["api_key"] = os.environ["MEMEX8_API_KEY"]

    return config


# ---------------------------------------------------------------------------
# HTTP Client
# ---------------------------------------------------------------------------

class _Client:
    """HTTP client for the memex8 REST API.

    Uses ``requests`` for robust timeout, retry, and error handling.
    """

    def __init__(self, api_key: str, base_url: str, timeout: float = _DEFAULT_TIMEOUT):
        self.api_key = api_key
        self.base_url = re.sub(r"/+$", "", base_url)
        self.timeout = timeout

    def _headers(self) -> dict:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

    def request(self, method: str, path: str, *, params=None, json_body=None, timeout: float = None) -> Any:
        import requests
        url = f"{self.base_url}{path}"
        t = timeout or self.timeout
        resp = requests.request(
            method.upper(), url,
            params=params,
            json=json_body if method.upper() not in {"GET", "DELETE"} else None,
            headers=self._headers(),
            timeout=t,
        )
        try:
            payload = resp.json()
        except Exception:
            payload = resp.text
        if not resp.ok:
            msg = ""
            if isinstance(payload, dict):
                msg = str(payload.get("message") or payload.get("error") or "")
            raise RuntimeError(f"memex8 {method} {path} failed ({resp.status_code}): {msg or payload}")
        return payload

    def search(self, query: str, top_k: int = 8, realm: str = None, min_score: float = 0.3) -> dict:
        body = {"query": query, "limit": top_k, "min_score": min_score}
        if realm:
            body["realm"] = realm
        return self.request("POST", "/api/v1/memories/search", json_body=body, timeout=_DEFAULT_SEARCH_TIMEOUT)

    def recall(self, top_k: int = 8, realm: str = None) -> dict:
        params = {"limit": top_k}
        if realm:
            params["realm"] = realm
        return self.request("GET", "/api/v1/memories/recall", params=params)

    def store(self, content: str, realm_hint: str = None, tags: list = None, source: str = None, memory_type: str = None) -> dict:
        body = {"content": content}
        if realm_hint:
            body["realm_hint"] = realm_hint
        if tags:
            body["tags"] = tags
        if source:
            body["source"] = source
        if memory_type:
            body["memory_type"] = memory_type
        return self.request("POST", "/api/v1/memories", json_body=body)

    def delete(self, memory_id: str) -> dict:
        return self.request("DELETE", f"/api/v1/memories/{memory_id}")

    def get_memory(self, memory_id: str) -> dict:
        return self.request("GET", f"/api/v1/memories/{memory_id}")

    def realms(self) -> dict:
        return self.request("GET", "/api/v1/realms")

    def stats(self) -> dict:
        return self.request("GET", "/api/v1/stats")

    def health(self) -> bool:
        import requests
        try:
            resp = requests.get(f"{self.base_url}/health", timeout=5.0)
            return resp.ok
        except Exception:
            return False

    def ingest_conversation(
        self, summary: str, source: str = "hermes", platform: str = "cli",
        tags: Optional[List[str]] = None,
    ) -> Optional[str]:
        """Send conversation summary via webhook. Returns the archive id if the
        server returns one, else None. Returns False-y on transport failure.

        The bool→id change is intentional: pre-compress checkpoint API v2 wants
        a referent it can put into the summary prompt so a later recall can
        cite the checkpoint. We pass tags through as a list inside the JSON
        body for stores that surface them.
        """
        try:
            body: Dict[str, Any] = {"summary": summary, "source": source, "platform": platform}
            if tags:
                body["tags"] = tags
            resp_data = self.request(
                "POST", "/api/v1/webhooks/conversation",
                json_body=body,
                timeout=15.0,
            )
            # Server is allowed to return: {"id": "..."}, {"archive_id": "..."},
            # or {"ok": true}. All three are accepted.
            if isinstance(resp_data, dict):
                for key in ("id", "archive_id", "checkpoint_id", "uuid"):
                    if resp_data.get(key):
                        return str(resp_data[key])
            return None
        except Exception:
            return None


# ---------------------------------------------------------------------------
# Tool Schemas
# ---------------------------------------------------------------------------

SEARCH_SCHEMA = {
    "name": "memex8_search",
    "description": (
        "Search your long-term memory by meaning. Finds relevant facts, "
        "project context, and user preferences stored across all past sessions. "
        "Use this before making decisions to check existing knowledge."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "What to search for."},
            "top_k": {"type": "integer", "description": "Max results (default: 8, max: 20)."},
            "realm": {"type": "string", "description": "Filter to a specific realm (optional)."},
            "min_score": {"type": "number", "description": "Minimum similarity 0-1 (default: 0.3)."},
        },
        "required": ["query"],
    },
}

RECALL_SCHEMA = {
    "name": "memex8_recall",
    "description": (
        "Get high-importance memories — a summary of what matters most. "
        "Good for session startup context or when you need a refresher on "
        "the user's preferences and environment."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "top_k": {"type": "integer", "description": "Max results (default: 8, max: 20)."},
            "realm": {"type": "string", "description": "Filter to a specific realm (optional)."},
        },
    },
}

REMEMBER_SCHEMA = {
    "name": "memex8_remember",
    "description": (
        "Store a durable fact in long-term memory. Use when the user asks you "
        "to remember something, shares a preference, corrects you, or reveals "
        "important context about their environment or projects. Stored content "
        "persists across all future sessions and is searchable via memex8_search."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "content": {
                "type": "string",
                "description": "The fact, decision, or observation to remember. Be specific and concise."
            },
            "realm_hint": {
                "type": "string",
                "description": "Suggested realm: 'personal', 'environment', 'projects', 'troubleshooting'. Auto-assigned if omitted."
            },
        },
        "required": ["content"],
    },
}

FORGET_SCHEMA = {
    "name": "memex8_forget",
    "description": (
        "Delete a memory by ID. Use only when the user explicitly asks to "
        "forget something or when a stored fact is no longer accurate."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "memory_id": {"type": "string", "description": "Memory ID to delete."},
        },
        "required": ["memory_id"],
    },
}

REALMS_SCHEMA = {
    "name": "memex8_realms",
    "description": (
        "List all knowledge realms with memory counts. Shows how your "
        "memories are organized into topic clusters."
    ),
    "parameters": {
        "type": "object",
        "properties": {},
        "required": [],
    },
}

GET_SCHEMA = {
    "name": "memex8_get",
    "description": "Retrieve a specific memory by its ID.",
    "parameters": {
        "type": "object",
        "properties": {
            "memory_id": {"type": "string", "description": "Memory UUID to retrieve."},
        },
        "required": ["memory_id"],
    },
}


# ---------------------------------------------------------------------------
# Overlay formatter
# ---------------------------------------------------------------------------

def _build_context_block(
    recall_results: list,
    search_results: Optional[list] = None,
    *,
    already_injected: Optional[set] = None,
    char_budget: Optional[int] = None,
    priorities: Optional[Dict[str, float]] = None,
    sanitize: bool = False,
) -> str:
    """Format recall/search results as a context block for the system prompt.

    Feature 1 (per-session dedup): if ``already_injected`` is provided, skip
    memories whose ID is in the set. If ``char_budget`` is provided, drop the
    lowest-priority memories until the formatted block fits within the budget
    and record the kept IDs in ``already_injected``.

    Feature 3 (sanitize): if ``sanitize=True``, each memory's content is
    passed through ``_sanitize_for_prompt`` to strip prompt-injection
    vectors (template braces, unbalanced triple-backticks, javascript:/data:
    URLs) before being formatted into the block.
    """
    def memory_id(r: dict) -> str:
        # Prefer server-provided 'id'; fall back to content hash for legacy
        # payloads (older Qdrant records may lack an id field).
        mid = r.get("id") or r.get("memory_id") or ""
        if mid:
            return str(mid)
        content = r.get("content", "")
        return "hash:" + hashlib.sha1(content.encode("utf-8", errors="ignore")).hexdigest()[:16]

    lines: list[str] = []

    if recall_results:
        lines.append("[memex8 — Important Context]")
        # Phase 1: build candidate list with dedup applied
        candidates: list = []
        for r in recall_results[:5]:
            mid = memory_id(r)
            if already_injected is not None and mid in already_injected:
                continue
            candidates.append((mid, r))

        # Phase 2: sort by priority and apply char budget gate
        if char_budget is not None and candidates:
            def prio(item):
                mid, r = item
                if priorities is not None and mid in priorities:
                    return priorities[mid]
                return float(r.get("importance", r.get("score", 0.5)) or 0.5)

            candidates.sort(key=prio, reverse=True)
            kept: list[str] = []
            used = 0  # excludes the header "[memex8 — Important Context]\n"
            for mid, r in candidates:
                content = _sanitize_for_prompt((r.get("content") or "").strip(), sanitize)
                if len(content) > 200:
                    content = content[:197] + "..."
                realm = r.get("realm_name", r.get("realm", ""))
                line = f"- [{realm}] {content}"
                cost = len(line) + 1  # +1 for newline
                # Reserve room for trailing empty line below
                if used + cost + 1 > char_budget:
                    continue
                kept.append(line)
                used += cost
                if already_injected is not None:
                    already_injected.add(mid)
            lines.extend(kept)
        else:
            for mid, r in candidates:
                content = _sanitize_for_prompt((r.get("content") or "").strip(), sanitize)
                if len(content) > 200:
                    content = content[:197] + "..."
                realm = r.get("realm_name", r.get("realm", ""))
                lines.append(f"- [{realm}] {content}")
                if already_injected is not None:
                    already_injected.add(mid)
        lines.append("")

    if search_results:
        lines.append("[memex8 — Search Results]")
        for r in search_results[:5]:
            content = _sanitize_for_prompt((r.get("content") or "").strip(), sanitize)
            if len(content) > 200:
                content = content[:197] + "..."
            realm = r.get("realm_name", r.get("realm", ""))
            score = r.get("score", r.get("importance", 0))
            lines.append(f"- [{realm}] (score: {score:.2f}) {content}")
        lines.append("")

    return "\n".join(lines)


# ---------------------------------------------------------------------------
# MemoryProvider Implementation
# ---------------------------------------------------------------------------

class Memex8MemoryProvider(MemoryProvider):
    """memex8 vector memory with semantic search and auto-organizing realms.

    Lifecycle:
      initialize()     → health check, create HTTP client
      prefetch()       → return cached background search results
      queue_prefetch() → launch async recall for the next turn
      sync_turn()      → store conversation facts after each exchange
      handle_tool()    → dispatch memex8_search, memex8_remember, etc.
      on_session_end() → send full conversation summary via webhook
      on_memory_write()→ mirror built-in memory writes to memex8
      on_pre_compress()→ durable checkpoint of transcript before lossy rewrite (API v2)
    """

    # Opt in to pre-compress checkpoint API v2: we archive the transcript to memex8
    # before compression discards it, and raise on any failure (fail-closed). The host
    # routes require_checkpoint=True to us, and gates lossy rewrites on our success.
    pre_compress_checkpoint_api_version = 2

    def __init__(self):
        self._config: Optional[dict] = None
        self._client: Optional[_Client] = None
        self._client_lock = threading.Lock()
        self._hermes_home = ""
        self._session_id = ""
        self._turn_counter = 0
        self._session_turns: List[dict] = []

        # Prefetch caches
        self._recall_result: list = []
        self._prefetch_lock = threading.Lock()
        self._prefetch_thread: Optional[threading.Thread] = None
        self._sync_thread: Optional[threading.Thread] = None

        # Circuit breaker
        self._consecutive_failures = 0
        self._breaker_open_until = 0.0
        self._breaker_threshold = 5
        self._breaker_cooldown = 120  # seconds

        # Per-session dedup tracking (Feature 1)
        # Memory IDs already injected via system prompt this session, so we
        # don't repeat them across turns. Cleared on session start/end.
        self._injected_this_session: set[str] = set()
        self._dedup_lock = threading.Lock()

    # -- Core identity --

    @property
    def name(self) -> str:
        return "memex8"

    def is_available(self) -> bool:
        """Check if memex8 is configured (API key present). No network calls."""
        return bool(os.environ.get("MEMEX8_API_KEY"))

    def get_config_schema(self) -> List[Dict[str, Any]]:
        """Config fields for `hermes memory setup`."""
        return [
            {
                "key": "api_key",
                "description": "memex8 API key (from MEMEX8_API_KEY in .env)",
                "secret": True,
                "required": True,
                "env_var": "MEMEX8_API_KEY",
            },
            {
                "key": "base_url",
                "description": "memex8 REST API endpoint",
                "default": _DEFAULT_BASE_URL,
                "env_var": "MEMEX8_BASE_URL",
            },
            {
                "key": "auto_recall",
                "description": "Automatically recall memories before each turn",
                "default": "true",
                "choices": ["true", "false"],
            },
            {
                "key": "auto_sync",
                "description": "Automatically save conversation turns to memory",
                "default": "true",
                "choices": ["true", "false"],
            },
            {
                "key": "recall_top_k",
                "description": "Max memories returned per auto-recall",
                "default": str(_DEFAULT_RECALL_TOP_K),
            },
            # Feature 1: per-session injection dedup
            {
                "key": "dedup_injected_memories",
                "description": (
                    "Don't re-inject a memory that was already in the system "
                    "prompt earlier this session. Off by default to preserve "
                    "existing behavior."
                ),
                "default": "false",
                "choices": ["true", "false"],
            },
            {
                "key": "recall_max_inject_chars",
                "description": (
                    "Hard cap on the size of the [memex8 — Important Context] "
                    "block injected per turn. Lowest-priority memories are "
                    "dropped to fit. Ignored when dedup_injected_memories is off."
                ),
                "default": "4000",
            },
            # Feature 2: decision/evaluation memory-type promotion
            {
                "key": "promote_decisions",
                "description": (
                    "Promote turns containing decision verbs (decided, "
                    "shipped, deployed, etc.) to memory_type='decision' so "
                    "they decay slower. Off by default."
                ),
                "default": "false",
                "choices": ["true", "false"],
            },
            {
                "key": "promote_evaluations",
                "description": (
                    "Promote turns containing evaluation/learning verbs "
                    "(learned, noticed, realized, etc.) to "
                    "memory_type='fact' so they decay slower. Off by default."
                ),
                "default": "false",
                "choices": ["true", "false"],
            },
            # Feature 3: prompt-injection sanitization
            {
                "key": "sanitize_for_prompt",
                "description": (
                    "Sanitize memory content before it enters the system "
                    "prompt: neutralize {{template}}, ${template}, "
                    "unbalanced triple-backticks, and javascript:/vbscript:/"
                    "data:text/html URLs. Off by default to preserve "
                    "existing content fidelity."
                ),
                "default": "false",
                "choices": ["true", "false"],
            },
            # Feature 4: configurable system-prompt template
            {
                "key": "system_prompt_template",
                "description": (
                    "Optional. Override the default system-prompt preamble "
                    "with a custom template. Available {{variables}}: "
                    "tool_search, tool_remember, tool_recall, tool_realms, "
                    "tool_forget, tool_get, base_url, version. Leave empty "
                    "to use the built-in default."
                ),
                "default": "",
            },
        ]

    def save_config(self, values: Dict[str, Any], hermes_home: str) -> None:
        """Write non-secret config to ~/.hermes/memex8.json."""
        config_dir = Path(hermes_home)
        config_dir.mkdir(parents=True, exist_ok=True)
        config_path = config_dir / "memex8.json"

        existing = {}
        if config_path.exists():
            try:
                existing = json.loads(config_path.read_text())
            except Exception:
                pass

        existing.update(values)
        config_path.write_text(json.dumps(existing, indent=2), encoding="utf-8")

    # -- Circuit breaker --

    def _is_breaker_open(self) -> bool:
        if self._consecutive_failures < self._breaker_threshold:
            return False
        if time.monotonic() >= self._breaker_open_until:
            self._consecutive_failures = 0
            return False
        return True

    def _record_success(self):
        self._consecutive_failures = 0

    def _record_failure(self, msg: str = ""):
        self._consecutive_failures += 1
        if self._consecutive_failures >= self._breaker_threshold:
            self._breaker_open_until = time.monotonic() + self._breaker_cooldown
            logger.warning(
                "memex8 circuit breaker tripped after %d failures. "
                "Pausing for %ds. %s",
                self._consecutive_failures, self._breaker_cooldown, msg,
            )

    # -- Client --

    def _get_client(self) -> _Client:
        with self._client_lock:
            if self._client is not None:
                return self._client

            self._config = _load_config(self._hermes_home)
            base_url = self._config.get("base_url", _DEFAULT_BASE_URL)
            api_key = self._config.get("api_key", "")
            timeout = float(self._config.get("timeout", _DEFAULT_TIMEOUT))

            self._client = _Client(api_key, base_url, timeout)
            return self._client

    # -- Lifecycle --

    def initialize(self, session_id: str, **kwargs) -> None:
        """Initialize for a session. Verify connectivity to memex8."""
        self._session_id = session_id
        self._hermes_home = kwargs.get("hermes_home", os.path.expanduser("~/.hermes"))
        self._turn_counter = 0
        self._session_turns = []
        self._recall_result = []
        # Feature 1: reset per-session dedup state on every new session
        with self._dedup_lock:
            self._injected_this_session = set()

        self._config = _load_config(self._hermes_home)
        base_url = self._config.get("base_url", _DEFAULT_BASE_URL)
        api_key = self._config.get("api_key", "")
        timeout = float(self._config.get("timeout", _DEFAULT_TIMEOUT))

        self._client = _Client(api_key, base_url, timeout)

        # Health check (non-blocking)
        def _health():
            if self._client.health():
                logger.info("memex8 connected at %s", base_url)
                self._record_success()
            else:
                logger.warning("memex8 health check failed at %s", base_url)
                self._record_failure("health check")

        t = spawn_context_thread(_health, name="memex8-health")
        t.start()

    def system_prompt_block(self) -> str:
        """Return the static preamble added to the system prompt.

        Feature 4: if the user has set ``system_prompt_template`` in
        ~/.hermes/memex8.json, render that template with the available
        {{variables}} (``tool_search``, ``tool_remember``, ``tool_recall``,
        ``tool_realms``, ``tool_forget``, ``tool_get``, ``base_url``,
        ``version``). Otherwise return the default 4-line block — zero
        behavior change for users who don't customize.
        """
        if not self._config or not self._config.get("system_prompt_template"):
            return _DEFAULT_SYSTEM_PROMPT_BLOCK

        template = self._config["system_prompt_template"]
        if not isinstance(template, str) or not template.strip():
            return _DEFAULT_SYSTEM_PROMPT_BLOCK

        base_url = self._config.get("base_url", _DEFAULT_BASE_URL)
        try:
            return _render_system_prompt(template, base_url)
        except Exception as e:
            # Bad template — fall back to default, log warning
            logger.warning(
                "memex8 system_prompt_template render failed: %s — using default", e
            )
            return _DEFAULT_SYSTEM_PROMPT_BLOCK

    # -- Background prefetch --

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        """Prefetch recall results in background for the next turn."""
        if self._is_breaker_open():
            return

        auto_recall = self._config.get("auto_recall", True) if self._config else True
        if not auto_recall:
            return

        def _run():
            try:
                client = self._get_client()
                top_k = int(self._config.get("recall_top_k", _DEFAULT_RECALL_TOP_K)) if self._config else _DEFAULT_RECALL_TOP_K
                results = client.recall(top_k=top_k)
                # Handle both dict-wrapped and direct-list responses
                if isinstance(results, dict):
                    recall_data = results.get("results", results.get("memories", []))
                elif isinstance(results, list):
                    recall_data = results
                else:
                    recall_data = []
                with self._prefetch_lock:
                    self._recall_result = recall_data if recall_data else []
                self._record_success()
            except Exception as e:
                self._record_failure(str(e))
                logger.debug("memex8 prefetch failed: %s", e)

        if self._prefetch_thread and self._prefetch_thread.is_alive():
            self._prefetch_thread.join(timeout=3.0)

        self._prefetch_thread = spawn_context_thread(
            _run, name="memex8-prefetch"
        )
        self._prefetch_thread.start()

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        """Return prefetched context block.

        Feature 1: when ``dedup_injected_memories`` is enabled in config,
        skip memories that have already been injected this session (cleared
        on ``initialize``/``on_session_end``). When ``recall_max_inject_chars``
        is set, additionally sort candidates by importance and drop
        lowest-priority memories to fit within the budget.
        """
        if self._prefetch_thread and self._prefetch_thread.is_alive():
            self._prefetch_thread.join(timeout=3.0)

        with self._prefetch_lock:
            recall = self._recall_result
            self._recall_result = []

        if not recall:
            return ""

        # Feature 1 — read config-gated dedup state
        cfg = self._config or {}
        dedup_enabled = bool(cfg.get("dedup_injected_memories", False))
        char_budget_raw = cfg.get("recall_max_inject_chars")
        try:
            char_budget = int(char_budget_raw) if char_budget_raw not in (None, "") else None
        except (TypeError, ValueError):
            char_budget = None
        # Feature 3 — read config-gated prompt-injection sanitization
        sanitize_enabled = bool(cfg.get("sanitize_for_prompt", False))

        already_injected = self._injected_this_session if dedup_enabled else None
        if dedup_enabled and char_budget is not None:
            # Annotate priorities by `importance` field (fallback `score`)
            priorities: Dict[str, float] = {}
            for r in recall[:5]:
                mid = str(r.get("id") or r.get("memory_id") or "") or (
                    "hash:" + hashlib.sha1(
                        (r.get("content") or "").encode("utf-8", errors="ignore")
                    ).hexdigest()[:16]
                )
                priorities[mid] = float(r.get("importance", r.get("score", 0.5)) or 0.5)

            with self._dedup_lock:
                block = _build_context_block(
                    recall,
                    already_injected=already_injected,
                    char_budget=char_budget,
                    priorities=priorities,
                    sanitize=sanitize_enabled,
                )
        elif dedup_enabled:
            with self._dedup_lock:
                block = _build_context_block(
                    recall, already_injected=already_injected, sanitize=sanitize_enabled
                )
        else:
            block = _build_context_block(recall, sanitize=sanitize_enabled)
        return f"## memex8 Memory (persistent context)\n{block}" if block else ""

    # -- Turn sync --

    def _classify_memory_type(self, user_content: str, assistant_content: str) -> str:
        """Return the memex8 ``memory_type`` for a turn based on content signals.

        Feature 2: when ``promote_decisions`` or ``promote_evaluations`` is
        enabled in config, turns matching decision/evaluation regexes get
        promoted to slower-decaying memory types (which gives them a
        longer Weibull lifetime). Otherwise returns the default ``"event"``.

        Existing users (both flags off) see no change.
        """
        if not self._config:
            return "event"
        promote_dec = bool(self._config.get("promote_decisions", False))
        promote_eval = bool(self._config.get("promote_evaluations", False))
        if not promote_dec and not promote_eval:
            return "event"
        combined = f"{user_content or ''}\n{assistant_content or ''}"
        if promote_dec and _DECISION_RE.search(combined):
            return "decision"  # k=1.00, scale=2wk — decays after acted on
        if promote_eval and _EVAL_RE.search(combined):
            return "fact"  # k=0.80, scale=1mo — durable
        return "event"

    def sync_turn(self, user_content: str, assistant_content: str, *, session_id: str = "") -> None:
        """Auto-store conversation turns as memories (non-blocking)."""
        if self._is_breaker_open():
            return

        auto_sync = self._config.get("auto_sync", True) if self._config else True
        if not auto_sync:
            return

        # Skip trivial messages
        if not user_content or _TRIVIAL_RE.match(user_content.strip()):
            return

        # Skip if too short
        if len(user_content.strip()) < 15:
            return

        self._turn_counter += 1

        # Accumulate turns for session-end summary
        self._session_turns.append({
            "role": "user",
            "content": user_content[:300],
        })
        self._session_turns.append({
            "role": "assistant",
            "content": assistant_content[:300],
        })

        def _sync():
            try:
                client = self._get_client()
                content = f"## User\n\n{user_content.strip()[:300]}\n\n## Assistant\n\n{assistant_content.strip()[:300]}"
                memory_type = self._classify_memory_type(user_content, assistant_content)
                client.store(
                    content,
                    realm_hint="conversations",
                    tags=["conversation", "auto-stored", f"type:{memory_type}"],
                    source="hermes-sync",
                    memory_type=memory_type,
                )
                self._record_success()
            except Exception as e:
                self._record_failure(str(e))
                logger.debug("memex8 sync_turn failed: %s", e)

        # Wait for previous sync before starting a new one
        if self._sync_thread and self._sync_thread.is_alive():
            self._sync_thread.join(timeout=5.0)

        self._sync_thread = spawn_context_thread(
            _sync, name="memex8-sync"
        )
        self._sync_thread.start()

    # -- Session end --

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        """Send full conversation summary to memex8 via webhook.

        Called when a session ends (exit, /reset, timeout).
        """
        # Feature 1: clear per-session dedup state on session end
        with self._dedup_lock:
            self._injected_this_session = set()
        if not messages or self._is_breaker_open():
            return

        def _ingest():
            try:
                client = self._get_client()

                # Build summary from the full message history
                summary_parts = []
                user_turns = 0
                for msg in messages:
                    role = msg.get("role", "")
                    content = msg.get("content", "")
                    if isinstance(content, str) and len(content.strip()) > 0:
                        preview = content[:200]
                        if role == "user":
                            user_turns += 1
                            summary_parts.append(f"User: {preview}")
                        elif role in ("assistant", "ai"):
                            summary_parts.append(f"Assistant: {preview}")

                if not summary_parts:
                    return

                summary = "\n".join(summary_parts)
                archive_id = client.ingest_conversation(
                    summary=summary,
                    source="hermes",
                    platform="cli",
                )
                if archive_id is not None:
                    self._record_success()
                    logger.info(
                        "memex8 session-end: ingested %d turns, %d chars (id=%s)",
                        user_turns, len(summary), archive_id,
                    )
            except Exception as e:
                self._record_failure(str(e))
                logger.debug("memex8 on_session_end failed: %s", e)

        t = spawn_context_thread(_ingest, name="memex8-session-end")
        t.start()

    # -- Pre-compress checkpoint (API v2: fail-closed durable archive) --

    def on_pre_compress(
        self,
        messages: List[Dict[str, Any]],
        *,
        require_checkpoint: bool = False,
    ) -> str:
        """Archive the transcript to memex8 before Hermes performs a lossy rewrite.

        Pre-compress checkpoint API v2. The host calls this just before compression
        discards the full transcript. With ``require_checkpoint=True`` (operator
        enabled ``compression.checkpoint_required: true``), a failure here raises
        and BLOCKS the lossy rewrite — the uncompressed transcript is preserved
        until the store recovers.

        With ``require_checkpoint=False`` (default), failures are logged and
        compression proceeds. This matches the doc's stated default for backends
        whose primary job is insight extraction, but for memex8 — a durable
        archive — we treat the checkpoint as load-bearing: archive synchronously,
        return the archive id so the host can include it in the summary prompt,
        and raise on any failure when ``require_checkpoint`` is set.

        Returns:
            A short marker string ``checkpoint: <archive_id>`` (or ``""`` when
            the call is non-load-bearing and the archive skipped).

        Raises:
            RuntimeError: when ``require_checkpoint`` is True and the archive
                could not be durably committed. The host catches this and blocks
                the lossy rewrite.
        """
        if not messages:
            return ""

        # Circuit-breaker short-circuit: don't even try when the store is known
        # unhealthy. With require_checkpoint, the operator explicitly opted into
        # fail-closed behavior — bypass the breaker so a single bad outage
        # doesn't silently trigger the rewrite.
        if self._is_breaker_open() and not require_checkpoint:
            return ""

        # Build the durable summary we want to archive. Use the same shape as
        # on_session_end so a later recall finds it consistently. The host passes
        # ``evidence_messages`` (normalized) to checkpoint providers — we accept
        # the raw list to keep this plugin portable across host versions.
        summary_parts: List[str] = []
        user_turns = 0
        for msg in messages:
            role = msg.get("role", "")
            content = msg.get("content", "")
            if isinstance(content, str) and content.strip():
                preview = content[:500]
                if role == "user":
                    user_turns += 1
                    summary_parts.append(f"User: {preview}")
                elif role in ("assistant", "ai"):
                    summary_parts.append(f"Assistant: {preview}")

        if not summary_parts:
            return ""

        summary = "\n".join(summary_parts)

        # SYNCHRONOUS archive. Unlike on_session_end (which is best-effort,
        # background), checkpoint semantics demand that the archive is durable
        # before we return — so the host can include the id in the summary
        # prompt and the lossy rewrite only proceeds on our success.
        try:
            client = self._get_client()
            archive_id = client.ingest_conversation(
                summary=summary,
                source="hermes-pre-compress-checkpoint",
                platform="cli",
                tags=["pre-compress-checkpoint", f"turns:{user_turns}"],
                # Synchronous path: ingest_conversation should return the id
                # when the underlying memex8 store supports it; for stores that
                # return only a bool, fall back to a synthetic id.
            )
        except Exception as e:
            self._record_failure(str(e))
            if require_checkpoint:
                logger.error(
                    "memex8 pre-compress checkpoint FAILED (blocking compression): %s", e
                )
                raise RuntimeError(
                    f"memex8 pre-compress checkpoint failed: {e}"
                ) from e
            logger.debug("memex8 pre-compress archive failed (non-blocking): %s", e)
            return ""

        # Success — record and return a marker. The host forwards this into the
        # summary prompt so a future recall can cross-reference the checkpoint.
        self._record_success()
        marker = f"checkpoint: {archive_id}" if archive_id else "checkpoint: archived"
        logger.info(
            "memex8 pre-compress checkpoint: archived %d turns (%d chars)",
            user_turns, len(summary),
        )
        return marker

    # -- Memory write mirror --

    def on_memory_write(self, action: str, target: str, content: str) -> None:
        """Mirror built-in memory writes (MEMORY.md / USER.md) to memex8."""
        if action != "add" or not content or self._is_breaker_open():
            return

        try:
            client = self._get_client()
            memory_type = "preference" if target == "user" else "factual"
            client.store(
                content,
                realm_hint=f"user-{target}",
                tags=[memory_type, "mirrored-from-builtin"],
                source="hermes-memory-tool",
            )
            self._record_success()
        except Exception as e:
            self._record_failure(str(e))
            logger.debug("memex8 memory mirror failed: %s", e)

    # -- Tools --

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        return [SEARCH_SCHEMA, RECALL_SCHEMA, REMEMBER_SCHEMA, FORGET_SCHEMA, REALMS_SCHEMA, GET_SCHEMA]

    def handle_tool_call(self, tool_name: str, args: dict, **kwargs) -> str:
        if self._is_breaker_open():
            return json.dumps({
                "error": "memex8 temporarily unavailable (multiple consecutive failures). "
                         "Will retry automatically."
            })

        try:
            client = self._get_client()
        except Exception as e:
            return tool_error(f"memex8 client unavailable: {e}")

        try:
            result = self._dispatch(client, tool_name, args)
            self._record_success()
            return json.dumps(result, ensure_ascii=False)
        except Exception as e:
            self._record_failure(str(e))
            return tool_error(str(e))

    def _dispatch(self, client: _Client, tool_name: str, args: dict) -> Any:
        if tool_name == "memex8_search":
            query = args.get("query", "")
            if not query:
                return {"error": "query is required"}
            results = client.search(
                query,
                top_k=min(int(args.get("top_k", _DEFAULT_SEARCH_TOP_K)), 20),
                realm=args.get("realm"),
                min_score=float(args.get("min_score", _DEFAULT_RECALL_MIN_SCORE)),
            )
            return results.get("results", results) if isinstance(results, dict) else results

        if tool_name == "memex8_recall":
            results = client.recall(
                top_k=min(int(args.get("top_k", _DEFAULT_RECALL_TOP_K)), 20),
                realm=args.get("realm"),
            )
            return results.get("results", results) if isinstance(results, dict) else results

        if tool_name == "memex8_remember":
            content = args.get("content", "")
            if not content:
                return {"error": "content is required"}
            return client.store(
                content,
                realm_hint=args.get("realm_hint"),
                source="hermes-tool",
            )

        if tool_name == "memex8_forget":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return client.delete(memory_id)

        if tool_name == "memex8_realms":
            return client.realms()

        if tool_name == "memex8_get":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return client.get_memory(memory_id)

        return {"error": f"Unknown tool: {tool_name}"}

    # -- Shutdown --

    def shutdown(self) -> None:
        for t in (self._prefetch_thread, self._sync_thread):
            if t and t.is_alive():
                t.join(timeout=5.0)
        with self._client_lock:
            self._client = None


# ---------------------------------------------------------------------------
# Plugin Registration
# ---------------------------------------------------------------------------

def register(ctx) -> None:
    """Register memex8 as a memory provider plugin."""
    ctx.register_memory_provider(Memex8MemoryProvider())
