"""memex8 memory plugin — MemoryProvider interface for Hermes Agent.

Self-hosted vector memory with semantic search, auto-organizing knowledge
realms, and TurboQuant compression.

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
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider, spawn_context_thread
from tools.registry import tool_error

from .client import _DEFAULT_SEARCH_TIMEOUT, _DEFAULT_TIMEOUT, _Client, _valid_receipt
from .redaction import _PrivateRedactor

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_DEFAULT_BASE_URL = "http://localhost:8080"
_DEFAULT_RECALL_TOP_K = 8
_DEFAULT_SEARCH_TOP_K = 8
_DEFAULT_RECALL_INJECT_CAP = 5  # hard cap on memories actually injected into the prompt
_DEFAULT_RECALL_MIN_SCORE = 0.3
_DEFAULT_RECALL_DEADLINE_MS = 1500.0
# Plugin version (kept in sync with plugin.yaml) — exposed via
# {{version}} in the configurable system_prompt_block template
_PLUGIN_VERSION = "1.2.0"

# Trivial messages that shouldn't be synced to memory
_TRIVIAL_RE = re.compile(
    r"^(ok|okay|thanks|thank you|got it|sure|yes|no|yep|nope|k|ty|thx|np)\.?$",
    re.IGNORECASE,
)

# Auto-generated cron / A2A / agent-internal messages that get passed in as
# user_content but are not user-authored text. Storing these as memories
# pollutes the corpus with hundreds of near-identical summaries of cron
# runs, drowning real memories in recall. The match is anchored at the
# start of the (stripped) message so genuine user messages that happen to
# contain these substrings later still go through.
_CRON_MARKER_RE = re.compile(
    r"^\s*\[("
    r"IMPORTANT:\s*You\s+are\s+running\s+as\s+a\s+scheduled\s+cron\s+job"
    r"|SILENT"
    r"|CRON_FAILURE"
    r"|A2A\s+inbound"
    r"|deliver.*the\s+output\s+yourself"  # the "do not use send_message" variant
    r"|assistant\s+reply\s+was\s+empty"
    r")",
    re.IGNORECASE,
)

# Hermes prepends this hint to every scheduled cron prompt. Searched (not
# anchored) because skill-loading jobs can put skill text ahead of it.
_CRON_HINT_RE = re.compile(
    r"\[IMPORTANT:\s*You\s+are\s+running\s+as\s+a\s+scheduled\s+cron\s+job",
    re.IGNORECASE,
)
# Silence tokens Hermes' cron lane honours (gateway.response_filters).
_CRON_SILENCE_TOKENS = frozenset({"[SILENT]", "SILENT", "NO_REPLY", "NO REPLY"})
_CRON_FAILURE_TOKEN = "[CRON_FAILURE]"
_CRON_FINDING_MAX_CHARS = 2000


def _is_cron_turn(user_content: str, session_id: str = "") -> bool:
    """True when the turn is a scheduled cron run rather than a user message."""
    if session_id and session_id.startswith("cron_"):
        return True
    return bool(user_content and _CRON_HINT_RE.search(user_content))


def _cron_finding(assistant_content: str) -> Optional[str]:
    """Return the cron run's reportable output, or None when there is nothing
    worth remembering.

    Mirrors Hermes' autonomous-lane silence rule: a silence token as the whole
    response, its first line, or its last line (or a response opening with the
    bracketed ``[SILENT]``) means "nothing new". ``[CRON_FAILURE]`` runs are
    about the job itself, not a finding, so they are dropped too.
    """
    text = (assistant_content or "").strip()
    if not text:
        return None
    lines = [ln.strip() for ln in text.splitlines() if ln.strip()]
    if text.upper().startswith("[SILENT]"):
        return None
    if any(c.upper() in _CRON_SILENCE_TOKENS for c in (text, lines[0], lines[-1])):
        return None
    if lines[0] == _CRON_FAILURE_TOKEN:
        return None
    return text[:_CRON_FINDING_MAX_CHARS]

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
# Archive transport limits
# ---------------------------------------------------------------------------

# One webhook write becomes one embedding input: the server embeds the whole
# ``summary`` string in a single request, and the embedding model caps one
# input at 8191 tokens. Chunks are therefore bounded in BYTES with margin for
# the densest realistic text — CJK runs ~1 token per character (3 bytes/char)
# and base64/hex tool output ~2 characters per token — so 8000 bytes stays far
# under the cap in every case. A 24000-byte CJK chunk is ~8000 tokens of a
# single string: one 400 there fails that chunk, hence the whole checkpoint,
# and with ``compression.checkpoint_required`` that blocks compression for good.
_WEBHOOK_CHUNK_BYTES = 8000

# The webhook stores what it is sent inside this framing
# (src/api/routes/webhook.rs). Every manifest page records it so reassembly
# strips it exactly instead of guessing.
_WEBHOOK_FRAMING_PREFIX = "# Conversation Summary\n\n"
_WEBHOOK_FRAMING_SUFFIX = "\n\n"

_MANIFEST_FORMAT = "memex8-transcript-manifest-v1"

# Checkpoint archives are evidence, not chatter: tag them so recall can find
# them and they are distinguishable from ordinary conversation memory, and
# store them as ``artifact`` (Weibull k=0.75/eta=2160h => ~273 days to reach the
# default prune score) instead of the webhook's ``general`` (~7 days, at which
# point slumber drops importance to ~0).
_ARCHIVE_TAGS = ("checkpoint", "transcript-archive")
_ARCHIVE_MEMORY_TYPE = "artifact"


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

def _transcript(messages, redactor: Optional["_PrivateRedactor"] = None) -> str:
    """Serialize the complete ordered evidence, redacting ``<private>`` spans.

    ``redactor`` carries span depth owned by the caller: an archive window can
    begin inside a span opened by an earlier window, and a fresh redactor would
    only see the closing tag and keep the plaintext.
    """
    # Reject unsupported objects instead of silently dropping their evidence.
    normalized = json.loads(json.dumps(messages, ensure_ascii=False, allow_nan=False))
    return json.dumps((redactor or _PrivateRedactor()).value(normalized),
                      ensure_ascii=False, separators=(",", ":"))


def _slice_bytes(raw: bytes, start: int, limit: int) -> int:
    """End offset of the next chunk: at most ``limit`` bytes, rune-aligned."""
    end = min(start + limit, len(raw))
    while end > start and end < len(raw) and raw[end] & 0xC0 == 0x80:
        end -= 1
    if end <= start:
        raise ValueError("cannot cut transcript on a UTF-8 boundary")
    return end


def _manifest_page(level: int, chunks: list, total_bytes: int, total_sha: str) -> dict:
    return {
        "format": _MANIFEST_FORMAT, "level": level, "encoding": "utf-8",
        "chunks": chunks, "bytes": total_bytes, "sha256": total_sha,
        # Recorded so a reassembly is exact: the webhook wraps each write in
        # this framing before storing it (src/api/routes/webhook.rs).
        "framing": {"prefix": _WEBHOOK_FRAMING_PREFIX, "suffix": _WEBHOOK_FRAMING_SUFFIX},
    }


def _pack_pages(nodes: list, level: int, total_bytes: int, total_sha: str) -> list:
    """Group manifest nodes into pages that each fit one webhook write.

    Manifest pages are embedded too, so they obey the same byte budget as the
    transcript chunks: a fixed 64-entry fan-out grows its own JSON past the
    embedding input limit.
    """
    pages: list = []
    current: list = []
    for node in nodes:
        current.append(node)
        encoded = json.dumps(_manifest_page(level, current, total_bytes, total_sha),
                             separators=(",", ":"))
        if len(encoded.encode("utf-8")) <= _WEBHOOK_CHUNK_BYTES:
            continue
        current.pop()
        if not current:
            raise ValueError("manifest node exceeds the webhook write budget")
        pages.append(_manifest_page(level, current, total_bytes, total_sha))
        current = [node]
    if current:
        pages.append(_manifest_page(level, current, total_bytes, total_sha))
    return pages


def _unframe(stored: str, framing: Optional[dict] = None) -> str:
    """Undo the webhook's content wrapper using the framing the manifest recorded."""
    prefix = (framing or {}).get("prefix", _WEBHOOK_FRAMING_PREFIX)
    suffix = (framing or {}).get("suffix", _WEBHOOK_FRAMING_SUFFIX)
    if (stored.startswith(prefix) and stored.endswith(suffix)
            and len(stored) >= len(prefix) + len(suffix)):
        return stored[len(prefix):len(stored) - len(suffix)]
    return stored


def reassemble_transcript(manifest: dict, fetch) -> str:
    """Rebuild an archived transcript from stored memories, verifying hashes.

    ``fetch(memory_id)`` returns the stored memory content. Every node records
    the sha256 of the bytes that were sent, so a mismatch — including one caused
    by server-side redaction rewriting a span — raises instead of silently
    yielding a corrupted transcript.
    """
    framing = manifest.get("framing")
    parts = []
    for node in manifest.get("chunks") or []:
        body = _unframe(fetch(node["id"]), framing)
        if node.get("sha256"):
            digest = hashlib.sha256(body.encode("utf-8")).hexdigest()
            if digest != node["sha256"]:
                raise ValueError(f"archive node {node['id']} does not match its recorded hash")
        try:
            page = json.loads(body)
        except ValueError:
            page = None
        if isinstance(page, dict) and page.get("format") == _MANIFEST_FORMAT:
            parts.append(reassemble_transcript(page, fetch))
        else:
            parts.append(body)
    return "".join(parts)


def _default_config() -> dict:
    return {
        "base_url": _DEFAULT_BASE_URL,
        "api_key": "",
        "auto_recall": True,
        "auto_sync": True,
        "recall_top_k": _DEFAULT_RECALL_TOP_K,
        "search_top_k": _DEFAULT_SEARCH_TOP_K,
        "recall_min_score": _DEFAULT_RECALL_MIN_SCORE,
        "recall_deadline_ms": _DEFAULT_RECALL_DEADLINE_MS,
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

    # Env vars always take final precedence.
    # Accept both MEMEX8_BASE_URL (preferred) and MEMEX8_API_BASE
    # (legacy — was used by the Ex8-ca/Hermes-A2A plugin's
    # memex8_client.py). Prefer the new one; if only the old one is
    # set, use it and log a deprecation hint. See issue #12.
    if os.environ.get("MEMEX8_BASE_URL"):
        config["base_url"] = os.environ["MEMEX8_BASE_URL"]
    elif os.environ.get("MEMEX8_API_BASE"):
        config["base_url"] = os.environ["MEMEX8_API_BASE"]
        logger.warning(
            "MEMEX8_API_BASE is deprecated, use MEMEX8_BASE_URL instead "
            "(see https://github.com/Ex8-ca/memex8/issues/12)"
        )
    if os.environ.get("MEMEX8_API_KEY"):
        config["api_key"] = os.environ["MEMEX8_API_KEY"]

    return config


# HTTP client: client.py. Private-span redaction: redaction.py.

# ---------------------------------------------------------------------------
# Tool Schemas
# ---------------------------------------------------------------------------

def _coverage_first_line(coverage: dict) -> str:
    best = coverage.get("best_score")
    best = f"{best:.2f}" if isinstance(best, (int, float)) else "null"
    realms = ", ".join(str(r).replace("\n", " ").replace("\r", " ") for r in coverage.get("suggested_realms", [])) or "none"
    return (f"coverage={coverage.get('level', 'none')} best={best} "
            f"results_above_min={coverage.get('results_above_min', 0)} try realms: {realms} (retrieval heuristic)")


TIMELINE_SCHEMA = {
    "name": "memex8_timeline",
    "description": "Chronological compact neighbors around exactly one id, query, or RFC3339 at timestamp. Includes ID/query anchor. Time mode has no synthetic row. score=0; truncated=true indicates the 10,000-row scan budget. Use memex8_get for full bodies.",
    "parameters": {
        "type": "object",
        "properties": {
            "id": {"type": "string", "format": "uuid"},
            "query": {"type": "string", "minLength": 1},
            "at": {"type": "string", "format": "date-time"},
            "before": {"type": "integer", "minimum": 0, "maximum": 50, "default": 5},
            "after": {"type": "integer", "minimum": 0, "maximum": 50, "default": 5},
            "realm": {"type": "string"},
            "visibility": {"type": "string", "enum": ["private", "public"]},
        },
        "oneOf": [{"required": ["id"]}, {"required": ["query"]}, {"required": ["at"]}],
    },
}

SEARCH_SCHEMA = {
    "name": "memex8_search",
    "description": (
        "Search your long-term memory by meaning. Finds relevant facts, "
        "project context, and user preferences stored across all past sessions. "
        "Returns a retrieval-strength coverage heuristic, not proof of answerability. "
        "Use this before making decisions to check existing knowledge. "
        "Use compact=true, then memex8_get(ids=[...]) for the ones that matter."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "compact": {"type": "boolean", "default": False},
            "query": {"type": "string", "description": "What to search for."},
            "tags": {"type": "array", "items": {"type": "string"}, "description": "Require all tags."},
            "visibility": {"type": "string", "enum": ["private", "public"]},
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
        "the user's preferences and environment. "
        "Use compact=true, then memex8_get(ids=[...]) for the ones that matter."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "compact": {"type": "boolean", "default": False},
            "top_k": {"type": "integer", "description": "Max results (default: 8, max: 20)."},
            "realm": {"type": "string", "description": "Filter to a specific realm (optional)."},
        },
    },
}

REMEMBER_SCHEMA = {
    "name": "memex8_remember",
    "description": (
        "Text inside <private>...</private> is removed before embedding/storage. "
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
            "certified": {
                "type": "boolean", "default": False,
                "description": "Protect from automatic rewriting and decay. Set true only when the user explicitly asks to remember or confirm this fact; ordinary capture stays uncertified."
            },
            "memory_type": {"type": "string", "description": "Semantic type, e.g. profile, preference, fact, event."},
            "realm_hint": {
                "type": "string",
                "description": "Suggested realm: 'personal', 'environment', 'projects', 'troubleshooting'. Auto-assigned if omitted."
            },
            "visibility": {
                "type": "string",
                "enum": ["private", "public"],
                "default": "private",
                "description": (
                    "Whether this memory is eligible for cross-agent sharing "
                    "via the Hermes-A2A bridge. 'private' (default) never leaves "
                    "memex8. 'public' becomes shareable with peers the operator "
                    "has approved in ~/.hermes/a2a_bridge/public.yaml. Only set "
                    "this to 'public' when the user explicitly asks for the "
                    "memory to be shareable."
                ),
            },
        },
        "required": ["content"],
    },
}

RESTORE_DUPLICATE_SCHEMA = {
    "name": "memex8_restore_duplicate",
    "description": "Restore a removed duplicate from its slumber operation journal. Keeps the current winner and newer edits, applies 30-day dedup/consolidation cooldowns, and is safe to retry. Use an operation ID from the slumber report/log.",
    "parameters": {"type": "object", "properties": {"operation_id": {"type": "string"}},
                   "required": ["operation_id"]},
}

UNCONSOLIDATE_SCHEMA = {
    "name": "memex8_unconsolidate",
    "description": "Undo a consolidated summary from its durable archive. Restores missing originals, retains newer edits, and deletes the summary after restoration. Sources get a 30-day consolidation cooldown. Safe to retry a partial failure.",
    "parameters": {
        "type": "object",
        "properties": {"memory_id": {"type": "string", "description": "Summary/journal ID"}},
        "required": ["memory_id"],
    },
}

CERTIFY_SCHEMA = {
    "name": "memex8_certify",
    "description": "Protect a user-confirmed memory from automatic rewriting, pruning, decay, and verification. Only certify with explicit user intent. Set revoke=true to resume normal lifecycle.",
    "parameters": {
        "type": "object",
        "properties": {
            "memory_id": {"type": "string"},
            "revoke": {"type": "boolean", "default": False},
            "by": {"type": "string", "description": "Optional attribution label, not authenticated identity."},
            "note": {"type": "string", "description": "Optional certification note."},
        },
        "required": ["memory_id"],
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

DREAMS_SCHEMA = {
    "name": "memex8_dreams",
    "description": (
        "Review dreams memex8 generated from the user's memories. A dream is fiction woven from "
        "memories, not a memory; it never appears in search. action=pending returns the newest "
        "unreviewed dream (title, dream, waking note) and how many are waiting, or pending=0. Show "
        "it to the user and ask whether to keep it. action=keep marks it kept (remember=true also "
        "stores it as a hidden memory, only if the user asks). action=discard deletes it. Never "
        "keep or discard without the user's answer."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["pending", "keep", "discard"]},
            "dream_id": {"type": "string", "description": "Required for keep and discard."},
            "remember": {"type": "boolean", "default": False, "description": "keep only: also store the dream as a hidden memory."},
        },
        "required": ["action"],
    },
}

GET_SCHEMA = {
    "name": "memex8_get",
    "description": "Fetch full memories selected from compact search/recall. Pass memory_id (legacy id also accepted) or ids (1–50). Batch results preserve order and report unavailable IDs in not_found.",
    "parameters": {
        "type": "object",
        "properties": {
            "memory_id": {"type": "string", "minLength": 1},
            "id": {"type": "string", "minLength": 1, "description": "Alias for memory_id."},
            "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 50},
        },
        "oneOf": [{"required": ["memory_id"]}, {"required": ["id"]}, {"required": ["ids"]}],
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
        # Phase 1: build candidate list with dedup applied.
        # Hard-cap at _DEFAULT_RECALL_INJECT_CAP so a configured recall_top_k > cap
        # doesn't blow past the prompt's reserved block.
        candidates: list = []
        for r in recall_results[:_DEFAULT_RECALL_INJECT_CAP]:
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
        for r in search_results[:_DEFAULT_RECALL_INJECT_CAP]:
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
        self._sync_privacy = _PrivateRedactor()
        self._sync_privacy_session = None
        self._sync_privacy_lock = threading.Lock()
        # Span depth for ARCHIVE windows (on_pre_compress / on_session_end).
        # Separate from _sync_privacy: turns and transcripts are different
        # ordered streams, so consuming a tag in one must not affect the other.
        self._archive_privacy = {"session": None, "head": None,
                                 "start_depth": 0, "end_depth": 0}
        self._archive_privacy_lock = threading.Lock()

        # Prefetch caches
        self._recall_result: list = []
        self._prefetch_key = None
        self._prefetch_generation = 0
        self._prefetch_deadline = 0.0
        self._prefetch_slots = threading.BoundedSemaphore(2)
        self._prefetch_workers: set[threading.Thread] = set()
        self._prefetch_lock = threading.RLock()
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
        """Check if memex8 is configured (API key present). No network calls.

        Checks the environment var first, then falls back to the JSON config
        file — a user who configured everything in ``~/.hermes/memex8.json``
        without setting ``MEMEX8_API_KEY`` in the environment should still
        count as available (matches the documented config precedence).
        """
        if os.environ.get("MEMEX8_API_KEY"):
            return True
        # Fall back to the JSON config file if hermes_home is known.
        if self._hermes_home:
            try:
                cfg_path = Path(self._hermes_home) / "memex8.json"
                if cfg_path.exists():
                    cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
                    if bool(cfg.get("api_key")):
                        return True
            except (OSError, json.JSONDecodeError):
                pass
        return False

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
                "description": "Automatically upload turns, session archives and optional checkpoints; required checkpoints remain explicit requests",
                "default": "true",
                "choices": ["true", "false"],
            },
            {
                "key": "recall_top_k",
                "description": "Max memories returned per auto-recall",
                "default": str(_DEFAULT_RECALL_TOP_K),
            },
            {
                "key": "recall_deadline_ms",
                "description": "End-to-end auto-recall budget in milliseconds (including background time)",
                "default": str(_DEFAULT_RECALL_DEADLINE_MS),
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
            # Feature 5: export / import
            {
                "key": "enable_export_import",
                "description": (
                    "Let /memex8 export|import read and write arbitrary host "
                    "paths, and run even when the circuit breaker is open. "
                    "Disabled by default: paths are then confined to "
                    "$HERMES_HOME/plugin-data/memex8/. Only flip on "
                    "temporarily for backup/migration."
                ),
                "default": "false",
                "choices": ["true", "false"],
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
        with self._prefetch_lock, self._client_lock:
            self._initialize_session(session_id, **kwargs)

    def _initialize_session(self, session_id: str, **kwargs) -> None:
        """Replace lifecycle state atomically with respect to submissions."""
        self._invalidate_prefetch()
        self._session_id = session_id
        self._hermes_home = kwargs.get("hermes_home", os.path.expanduser("~/.hermes"))
        self._turn_counter = 0
        self._session_turns = []
        with self._sync_privacy_lock:
            self._sync_privacy = _PrivateRedactor()
            self._sync_privacy_session = session_id
        with self._archive_privacy_lock:
            self._archive_privacy = {"session": session_id, "head": None,
                                     "start_depth": 0, "end_depth": 0}
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

    def _invalidate_prefetch(self) -> None:
        with self._prefetch_lock:
            self._prefetch_generation += 1
            self._prefetch_key = None
            self._recall_result = []

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        """Queue query-aware, best-effort recall without waiting for old work.

        At most two HTTP workers can be inflight. Overload drops the new
        speculative request and invalidates obsolete results; no backlog is
        retained. Explicit memory tools are unaffected.
        """
        with self._prefetch_lock:
            submission = self._prepare_prefetch(query, session_id=session_id)
        if submission is None:
            return
        generation, deadline = submission

        # Client creation may block: never hold the lifecycle lock across it.
        # An initialize during capture makes this submission obsolete; recheck
        # before snapshotting config or allowing any worker to dispatch.
        try:
            client = self._get_client()
            with self._prefetch_lock:
                if generation != self._prefetch_generation:
                    self._prefetch_slots.release()
                    return
                cfg = dict(self._config or {})
            top_k = int(cfg.get("recall_top_k", _DEFAULT_RECALL_TOP_K))
            min_score = float(cfg.get("recall_min_score", _DEFAULT_RECALL_MIN_SCORE))
        except Exception as exc:
            self._prefetch_slots.release()
            logger.debug("memex8 prefetch setup failed: %s", exc)
            return

        self._start_prefetch(query, generation, deadline, client, top_k, min_score)

    def _prepare_prefetch(self, query: str, *, session_id: str):
        """Reserve a generation and slot while holding the lifecycle lock."""
        if self._is_breaker_open():
            self._invalidate_prefetch()
            return

        auto_recall = self._config.get("auto_recall", True) if self._config else True
        if not auto_recall:
            self._invalidate_prefetch()
            return

        key = (session_id or self._session_id, query)
        try:
            budget_ms = float((self._config or {}).get("recall_deadline_ms", _DEFAULT_RECALL_DEADLINE_MS))
            if not 0 < budget_ms < float("inf"):
                raise ValueError("invalid recall deadline")
        except (TypeError, ValueError):
            budget_ms = _DEFAULT_RECALL_DEADLINE_MS
        deadline = time.monotonic() + budget_ms / 1000.0
        with self._prefetch_lock:
            self._prefetch_generation += 1
            generation = self._prefetch_generation
            self._prefetch_key = key
            self._prefetch_deadline = deadline
            self._recall_result = []
            if not self._prefetch_slots.acquire(blocking=False):
                # Best-effort recall: supersede obsolete work without creating
                # a backlog of network requests or waiting for old workers.
                return

        return generation, deadline

    def _start_prefetch(self, query, generation, deadline, client, top_k, min_score):
        def _run():
            try:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return
                results = client.search(query, top_k=top_k, min_score=min_score, timeout=remaining)
                # Handle both dict-wrapped and direct-list responses
                if isinstance(results, dict):
                    recall_data = results.get("results", results.get("memories", []))
                elif isinstance(results, list):
                    recall_data = results
                else:
                    recall_data = []
                if not isinstance(recall_data, list):
                    raise ValueError("prefetch results must be a list")
                for entry in recall_data:
                    if not isinstance(entry, dict) or not isinstance(entry.get("content"), str):
                        raise ValueError("prefetch memory must have string content")
                    for field in ("importance", "score"):
                        if field in entry:
                            value = entry[field]
                            if not isinstance(value, (int, float, str)):
                                raise ValueError("invalid prefetch memory priority")
                            float(value)
                with self._prefetch_lock:
                    if generation == self._prefetch_generation and time.monotonic() <= deadline:
                        self._recall_result = recall_data if recall_data else []
                        self._record_success()
            except Exception as e:
                with self._prefetch_lock:
                    if generation == self._prefetch_generation:
                        self._record_failure(str(e))
                logger.debug("memex8 prefetch failed: %s", e)
            finally:
                self._prefetch_slots.release()

        try:
            with self._prefetch_lock:
                if generation != self._prefetch_generation:
                    self._prefetch_slots.release()
                    return
                self._prefetch_workers = {t for t in self._prefetch_workers if t.is_alive()}
                self._prefetch_thread = spawn_context_thread(_run, name="memex8-prefetch")
                self._prefetch_workers.add(self._prefetch_thread)
                self._prefetch_thread.start()
        except Exception as exc:
            self._prefetch_slots.release()
            logger.debug("memex8 prefetch setup failed: %s", exc)
            return

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        """Return prefetched context block.

        Feature 1: when ``dedup_injected_memories`` is enabled in config,
        skip memories that have already been injected this session (cleared
        on ``initialize``/``on_session_end``). When ``recall_max_inject_chars``
        is set, additionally sort candidates by importance and drop
        lowest-priority memories to fit within the budget.
        """
        with self._prefetch_lock:
            if self._prefetch_key != (session_id or self._session_id, query):
                return ""
            generation = self._prefetch_generation
            deadline = self._prefetch_deadline
            worker = self._prefetch_thread
        if worker and worker.is_alive():
            worker.join(timeout=max(0.0, deadline - time.monotonic()))

        with self._prefetch_lock:
            if generation != self._prefetch_generation:
                return ""
            recall = self._recall_result if time.monotonic() <= deadline else []
            self._prefetch_generation += 1
            self._prefetch_key = None
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
            for r in recall[:_DEFAULT_RECALL_INJECT_CAP]:
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

    def _automatic_storage_allowed(self) -> bool:
        value = (self._config or {}).get("auto_sync", True)
        return value is True or (isinstance(value, str) and value.strip().lower() == "true")

    def _archive_transcript(self, summary: str, source: str) -> str:
        """Commit bounded chunks, then a receipt manifest, via the existing webhook.

        Chunks are exact consecutive UTF-8 text slices, each small enough to
        survive as a single embedding input. The manifest is written last and is
        the only checkpoint referent. Partial writes are harmless orphan chunks,
        never a successful checkpoint. Server durability is its receipt
        contract; this client cannot prove disk flush independently.

        Every write carries archive provenance (tags + ``artifact`` memory
        type) so checkpoint evidence is durable and distinguishable from
        ordinary conversation memory instead of decaying with it.
        """
        client = self._get_client()

        def write(text):
            if len(text.encode("utf-8")) > _WEBHOOK_CHUNK_BYTES:
                # Refuse locally rather than eat a 400 for the whole checkpoint.
                raise RuntimeError("archive write exceeds the embedding input budget")
            receipt = client.ingest_conversation(
                summary=text, source=source, platform="cli",
                tags=list(_ARCHIVE_TAGS) + [f"source:{source}"],
                memory_type=_ARCHIVE_MEMORY_TYPE,
            )
            if not _valid_receipt(receipt):
                raise RuntimeError("archive missing durable receipt")
            return receipt

        raw = summary.encode("utf-8")
        if not raw:
            raise RuntimeError("refusing to archive an empty transcript")
        chunks = []
        offset = 0
        while offset < len(raw):
            end = _slice_bytes(raw, offset, _WEBHOOK_CHUNK_BYTES)
            part = raw[offset:end]
            chunks.append({"id": write(part.decode("utf-8")), "bytes": len(part),
                           "sha256": hashlib.sha256(part).hexdigest()})
            offset = end
        total_bytes = len(raw)
        total_sha = hashlib.sha256(raw).hexdigest()
        # Manifest pages keep arbitrarily large transcripts bounded too.
        nodes = chunks
        level = 0
        while True:
            parents = []
            for page in _pack_pages(nodes, level, total_bytes, total_sha):
                body = json.dumps(page, separators=(",", ":"))
                parents.append({"id": write(body), "bytes": len(body.encode("utf-8")),
                                "sha256": hashlib.sha256(body.encode("utf-8")).hexdigest()})
            if len(parents) == 1:
                return parents[0]["id"]
            nodes = parents
            level += 1

    def _archive_text(self, messages) -> str:
        """Serialize an archive window, continuing the session's span depth.

        The host hands ``on_pre_compress``/``on_session_end`` windows that can
        begin inside a ``<private>`` span opened earlier in the session, because
        compression rotates the session and later windows start after the cut.
        Redaction depth is therefore session state, like ``_sync_privacy``:
        carrying it across calls is what stops a mid-span window from archiving
        the span's plaintext. A window that re-sends one already seen (same
        leading message) starts at the same depth it started at before, so a
        cumulative re-send is not redacted twice.
        """
        head = messages[0] if messages else None
        fingerprint = hashlib.sha256(json.dumps(
            head, ensure_ascii=False, sort_keys=True, separators=(",", ":"),
        ).encode("utf-8")).hexdigest()
        key = self._session_id or ""
        with self._archive_privacy_lock:
            state = self._archive_privacy
            if state["session"] != key:
                state = {"session": key, "head": None, "start_depth": 0, "end_depth": 0}
                self._archive_privacy = state
            start_depth = state["start_depth"] if state["head"] == fingerprint else state["end_depth"]
            redactor = _PrivateRedactor()
            redactor.depth = start_depth
            state["head"] = fingerprint
            state["start_depth"] = start_depth
        text = _transcript(messages, redactor)
        with self._archive_privacy_lock:
            if self._archive_privacy["session"] == key:
                self._archive_privacy["end_depth"] = redactor.depth
        return text

    def sync_turn(self, user_content: str, assistant_content: str, *, session_id: str = "") -> None:
        """Auto-store turns; consume privacy delimiters BEFORE any skip/truncation."""
        key = session_id or self._session_id
        with self._sync_privacy_lock:
            if self._sync_privacy_session != key:
                self._sync_privacy = _PrivateRedactor()
                self._sync_privacy_session = key
            user_content = self._sync_privacy.text(user_content or "")
            assistant_content = self._sync_privacy.text(assistant_content or "")
        if not self._automatic_storage_allowed() or self._is_breaker_open():
            return

        # Skip trivial messages
        if not user_content or _TRIVIAL_RE.match(user_content.strip()):
            return

        # Scheduled cron run: keep only what the job actually found. The
        # prompt, [SILENT] runs and [CRON_FAILURE] runs are job mechanics,
        # not knowledge, and previously made up ~70% of the corpus.
        if _is_cron_turn(user_content, session_id or self._session_id):
            self._store_cron_finding(assistant_content, session_id or self._session_id)
            return

        # Skip auto-generated cron / A2A / agent-internal messages. These
        # look like user input to the plugin but aren't — storing them
        # produces hundreds of near-identical "summary of cron run"
        # memories that drown real memories in recall. See
        # `fix/prune-and-cron-filter`.
        if _CRON_MARKER_RE.match(user_content.strip()):
            logger.debug(
                "memex8 sync_turn skipped (cron/a2a marker): %.60s...",
                user_content.strip(),
            )
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
                if not self._automatic_storage_allowed():
                    return
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

    def _store_cron_finding(self, assistant_content: str, session_id: str = "") -> None:
        """Store a cron run's reportable output (never the prompt) as one memory."""
        finding = _cron_finding(assistant_content)
        if finding is None:
            logger.debug("memex8 cron turn skipped (silent/failure/empty)")
            return
        job_id = ""
        if session_id.startswith("cron_"):
            # cron_<job_id>_<YYYYmmdd>_<HHMMSS>
            job_id = session_id[len("cron_"):].rsplit("_", 2)[0]
        tags = ["cron-finding", "auto-stored"]
        if job_id:
            tags.append(f"cron-job:{job_id}")

        def _sync():
            try:
                if not self._automatic_storage_allowed():
                    return
                self._get_client().store(
                    finding,
                    realm_hint="cron-findings",
                    tags=tags,
                    source="hermes-cron",
                    memory_type="event",
                )
                self._record_success()
            except Exception as e:
                self._record_failure(str(e))
                logger.debug("memex8 cron finding store failed: %s", e)

        if self._sync_thread and self._sync_thread.is_alive():
            self._sync_thread.join(timeout=5.0)
        self._sync_thread = spawn_context_thread(_sync, name="memex8-cron-finding")
        self._sync_thread.start()

    # -- Session end --

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        """Send full conversation summary to memex8 via webhook.

        Called when a session ends (exit, /reset, timeout).
        """
        self._invalidate_prefetch()
        # Feature 1: clear per-session dedup state on session end
        with self._dedup_lock:
            self._injected_this_session = set()
        if not messages or not self._automatic_storage_allowed() or self._is_breaker_open():
            return

        # Cron sessions: the finding (if any) was already stored by sync_turn.
        # A session summary would just re-store the cron prompt.
        if _is_cron_turn("", self._session_id) or any(
            m.get("role") == "user"
            and isinstance(m.get("content"), str)
            and _CRON_HINT_RE.search(m["content"])
            for m in messages
        ):
            logger.debug("memex8 session-end skipped (cron session)")
            return

        # Snapshot and sanitize before the background worker can race host edits.
        try:
            summary = self._archive_text(messages)
        except Exception:
            self._record_failure("transcript normalization failed")
            return

        def _ingest():
            try:
                if not self._automatic_storage_allowed():
                    return
                self._archive_transcript(summary, "hermes")
                self._record_success()
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

        # Optional checkpoints obey automatic-upload consent. A required
        # checkpoint is an explicit operator request, independent of auto_sync.
        if not require_checkpoint and not self._automatic_storage_allowed():
            return ""

        # Circuit-breaker short-circuit: don't even try when the store is known
        # unhealthy. With require_checkpoint, the operator explicitly opted into
        # fail-closed behavior — bypass the breaker so a single bad outage
        # doesn't silently trigger the rewrite.
        if self._is_breaker_open() and not require_checkpoint:
            return ""

        # Cron sessions aren't worth archiving (findings go through sync_turn).
        # Only skip when the checkpoint is optional — skipping a required one
        # would block compression.
        if not require_checkpoint and (
            _is_cron_turn("", self._session_id)
            or any(
                m.get("role") == "user"
                and isinstance(m.get("content"), str)
                and _CRON_HINT_RE.search(m["content"])
                for m in messages
            )
        ):
            return ""

        # Normalize/redact the COMPLETE ordered evidence before chunking. All
        # roles, tool calls, structured blocks and metadata remain represented.
        try:
            summary = self._archive_text(messages)
            archive_id = self._archive_transcript(summary, "hermes-pre-compress-checkpoint")
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
        marker = f"checkpoint: {archive_id}"
        logger.info("memex8 pre-compress checkpoint: archived %d chars", len(summary))
        return marker

    # -- Export / Import (Feature 5) --

    def _export_import_enabled(self) -> bool:
        cfg = self._config or {}
        return str(cfg.get("enable_export_import", "false")).strip().lower() == "true"

    def resolve_bundle_path(self, raw: str) -> str:
        """Confine a chat-supplied export/import path to the plugin data dir
        unless the operator set ``enable_export_import`` to true.

        Returns the resolved absolute path. Rejects anything whose resolved
        location is not strictly inside ``base`` — including ``..`` segments,
        absolute paths, and symlinks that point outside (``resolve()``
        collapses symlink targets before the ancestry check).
        """
        if self._export_import_enabled():
            return raw
        base = (Path(self._hermes_home or os.path.expanduser("~/.hermes"))
                / "plugin-data" / "memex8").resolve()
        resolved = (base / raw).resolve()
        # `resolved.parents` starts at resolved's immediate parent, so a file
        # directly inside base passes, and anything deeper in the tree passes
        # too. Anything escaping base (upward traversal, abs paths, escaped
        # symlinks) does not.
        if base not in resolved.parents:
            raise ValueError(
                f"{raw}: outside {base}; set enable_export_import=true to use other paths"
            )
        # Create the full destination directory chain, not just the base —
        # nested paths like 'backups/weekly/dump.json' would otherwise pass
        # the confinement check and then fail on open() with FileNotFoundError
        # because 'backups/weekly/' was never created.
        resolved.parent.mkdir(parents=True, exist_ok=True)
        return str(resolved)

    def export_memories(
        self,
        output_path: str,
        *,
        realm: Optional[str] = None,
        page_size: int = 100,
    ) -> int:
        """Dump memories (optionally filtered by realm) to a JSON file.

        Feature 5. Walks the server's paginated ``GET /api/v1/memories``
        endpoint and writes a versioned bundle to ``output_path``. Returns
        the count of memories exported.

        The bundle shape:
            {
              "version": "1.1.0",
              "exported_at": "<ISO 8601 UTC>",
              "realm": "<realm filter or null>",
              "count": <int>,
              "page_size": <int>,
              "memories": [
                {<server's MemoryPoint dict>},
                ...
              ]
            }

        Notes:
        - Vectors are NOT exported. Re-import relies on the server
          re-embedding from content (deterministic given the model).
        - Writes are atomic: write to ``<output_path>.tmp`` then rename.
        - Caller is responsible for permissions / path safety.
        """
        if self._is_breaker_open() and not self._export_import_enabled():
            raise RuntimeError("memex8 circuit breaker is open; export cancelled")
        client = self._get_client()
        exported: List[Dict[str, Any]] = []
        offset = 0
        while True:
            resp = client.list_memories(
                limit=page_size, offset=offset, realm=realm
            )
            batch = resp.get("memories", []) if isinstance(resp, dict) else []
            if not batch:
                break
            exported.extend(batch)
            total = resp.get("total", len(exported)) if isinstance(resp, dict) else None
            offset += len(batch)
            # Stop if we've paginated past total (server reports it).
            if total is not None and offset >= total:
                break
            if len(batch) < page_size:
                break  # server returned a short page = end of results
        bundle = {
            "version": _PLUGIN_VERSION,
            "exported_at": datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
            "realm": realm,
            "count": len(exported),
            "page_size": page_size,
            "memories": exported,
        }
        tmp_path = output_path + ".tmp"
        try:
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(bundle, f, indent=2, ensure_ascii=False)
            os.replace(tmp_path, output_path)
        finally:
            if os.path.exists(tmp_path):
                try:
                    os.unlink(tmp_path)
                except OSError:
                    pass
        return len(exported)

    def import_memories(
        self,
        input_path: str,
        *,
        realm_hint: Optional[str] = None,
        dry_run: bool = False,
    ) -> int:
        """Re-store memories from a JSON bundle produced by export_memories().

        Feature 5. Idempotent: memories whose content exactly matches an
        existing memory (per the server's own dedup) are skipped. Returns
        the count actually imported (not skipped).

        Schema discipline:
        - Refuses to import bundles whose ``version`` is NEWER than this
          plugin (the plugin can't safely interpret newer fields).
        - Passes through realm / tags / memory_type / source / importance
          when present.
        """
        with open(input_path, "r", encoding="utf-8") as f:
            bundle = json.load(f)
        if not isinstance(bundle, dict) or "memories" not in bundle:
            raise ValueError(f"{input_path}: not a memex8 export bundle")
        exported_version = str(bundle.get("version", "0"))
        current_version = _PLUGIN_VERSION
        # Compare major.minor — refuse if newer than us.
        def _major_minor(v: str) -> tuple:
            parts = v.split(".")[:2]
            try:
                return (int(parts[0]), int(parts[1]) if len(parts) > 1 else 0)
            except (ValueError, IndexError):
                return (0, 0)
        if _major_minor(exported_version) > _major_minor(current_version):
            raise ValueError(
                f"{input_path}: bundle version {exported_version} is newer than "
                f"plugin version {current_version}; upgrade the plugin first"
            )
        client = self._get_client()
        imported = 0
        skipped = 0
        target_realm = realm_hint or bundle.get("realm")
        for mem in bundle["memories"]:
            if not isinstance(mem, dict):
                continue
            content = mem.get("content") or ""
            if not content:
                continue
            if dry_run:
                skipped += 1
                continue
            try:
                # Re-store. Pass through what we know about.
                # Round-trip preserves visibility so an exported
                # public memory stays public on re-import.
                kwargs = {
                    "realm_hint": mem.get("realm") or target_realm,
                    "tags": mem.get("tags") if isinstance(mem.get("tags"), list) else None,
                    "source": mem.get("source") or "imported",
                    "memory_type": mem.get("memory_type"),
                    "visibility": mem.get("visibility"),
                }
                # Filter out None values to match _Client.store's optional handling
                kwargs = {k: v for k, v in kwargs.items() if v is not None}
                client.store(content, **kwargs)
                imported += 1
            except Exception as e:
                logger.warning("memex8 import skipped memory %s: %s", mem.get("id"), e)
                skipped += 1
        return imported

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
        return [TIMELINE_SCHEMA, SEARCH_SCHEMA, RECALL_SCHEMA, REMEMBER_SCHEMA, CERTIFY_SCHEMA, UNCONSOLIDATE_SCHEMA, RESTORE_DUPLICATE_SCHEMA, FORGET_SCHEMA, REALMS_SCHEMA, GET_SCHEMA, DREAMS_SCHEMA]

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
            if tool_name == "memex8_search" and isinstance(result, dict) and isinstance(result.get("coverage"), dict):
                return _coverage_first_line(result["coverage"]) + "\n" + json.dumps(result, ensure_ascii=False)
            return json.dumps(result, ensure_ascii=False)
        except Exception as e:
            self._record_failure(str(e))
            return tool_error(str(e))

    def _dispatch(self, client: _Client, tool_name: str, args: dict) -> Any:
        if tool_name == "memex8_timeline":
            if sum(args.get(k) is not None for k in ("id", "query", "at")) != 1:
                return {"error": "Pass exactly one of id, query, or at"}
            for key in ("before", "after"):
                value = args.get(key, 5)
                if type(value) is not int or not 0 <= value <= 50:
                    return {"error": "before and after must be integers between 0 and 50"}
            for key in ("id", "query", "at", "realm"):
                if key in args and (not isinstance(args[key], str) or not args[key].strip()):
                    return {"error": key + " must be a nonempty string"}
            if "visibility" in args and args["visibility"] not in ("private", "public"):
                return {"error": "visibility must be private or public"}
            return client.timeline(**{k: args[k] for k in
                ("id", "query", "at", "before", "after", "realm", "visibility") if k in args})

        if tool_name == "memex8_search":
            compact = args.get("compact", False)
            if not isinstance(compact, bool):
                return {"error": "compact must be a boolean"}
            query = args.get("query", "")
            if not query:
                return {"error": "query is required"}
            results = client.search(
                query,
                top_k=min(int(args.get("top_k", _DEFAULT_SEARCH_TOP_K)), 20),
                realm=args.get("realm"),
                min_score=float(args.get("min_score", _DEFAULT_RECALL_MIN_SCORE)),
                **({"compact": True} if compact else {}),
                **({"tags": args["tags"]} if "tags" in args else {}),
                **({"visibility": args["visibility"]} if "visibility" in args else {}),
            )
            if isinstance(results, dict) and "coverage" in results:
                return results
            return results.get("results", results) if isinstance(results, dict) else results

        if tool_name == "memex8_recall":
            compact = args.get("compact", False)
            if not isinstance(compact, bool):
                return {"error": "compact must be a boolean"}
            results = client.recall(
                top_k=min(int(args.get("top_k", _DEFAULT_RECALL_TOP_K)), 20),
                realm=args.get("realm"),
                **({"compact": True} if compact else {}),
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
                memory_type=args.get("memory_type"),
                certified=args.get("certified", False),
                # Server treats None as "private"; pass through whatever
                # the model sent (including "public") so the schema's
                # enum constraint is the only gate.
                visibility=args.get("visibility"),
            )

        if tool_name == "memex8_restore_duplicate":
            operation_id = args.get("operation_id", "")
            if not operation_id:
                return {"error": "operation_id is required"}
            return client.restore_duplicate(operation_id)

        if tool_name == "memex8_unconsolidate":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return client.unconsolidate(memory_id)

        if tool_name == "memex8_certify":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return client.certify(memory_id, revoke=args.get("revoke", False),
                                  by=args.get("by"), note=args.get("note"))

        if tool_name == "memex8_dreams":
            action = args.get("action")
            if action == "pending":
                return client.dreams_pending()
            if action in ("keep", "discard"):
                dream_id = args.get("dream_id", "")
                if not dream_id:
                    return {"error": f"dream_id is required for {action}"}
                if action == "keep":
                    return client.dream_keep(dream_id, remember=args.get("remember", False))
                return client.dream_discard(dream_id)
            return {"error": "action must be one of: pending, keep, discard"}

        if tool_name == "memex8_forget":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return client.delete(memory_id)

        if tool_name == "memex8_realms":
            # The server drops centroids as of the realms-summary fix; strip
            # them here too so an older server can't flood the context.
            realms = client.realms()
            if isinstance(realms, list):
                realms = [
                    {k: v for k, v in r.items() if k != "centroid"} if isinstance(r, dict) else r
                    for r in realms
                ]
            return realms

        if tool_name == "memex8_get":
            if sum(key in args for key in ("memory_id", "id", "ids")) != 1:
                return {"error": "Pass exactly one of memory_id, id, or ids"}
            if "ids" in args:
                ids = args["ids"]
                if not isinstance(ids, list) or not 1 <= len(ids) <= 50 or not all(isinstance(id_, str) for id_ in ids):
                    return {"error": "ids must contain between 1 and 50 string memory IDs"}
                return client.batch_get(ids)
            memory_id = args.get("memory_id", args.get("id"))
            if not isinstance(memory_id, str) or not memory_id:
                return {"error": "memory_id must be a nonempty string"}
            return client.get_memory(memory_id)

        return {"error": f"Unknown tool: {tool_name}"}

    # -- Shutdown --

    def shutdown(self) -> None:
        self._invalidate_prefetch()
        with self._prefetch_lock:
            workers = list(self._prefetch_workers)
        deadline = time.monotonic() + 5.0
        for t in workers + [self._sync_thread]:
            if t and t.is_alive():
                t.join(timeout=max(0.0, deadline - time.monotonic()))
        with self._client_lock:
            self._client = None


# ---------------------------------------------------------------------------
# Slash command — Feature 6
# ---------------------------------------------------------------------------

_MEMEX8_HELP = """\
/memex8 — self-hosted vector memory for Hermes

Usage:
  /memex8 stats                  Show memory count, breaker state, recent errors
  /memex8 status                 Show config + connection status
  /memex8 search <query>         Run a semantic search and print top-5 hits
  /memex8 recall <query>         Force a high-importance recall and print results
  /memex8 remember <content>     Store a new memory (private by default)
  /memex8 remember --public <c> Store a new memory marked public (A2A-shareable)
  /memex8 forget <memory_id>     Delete a memory by id (irrevocable)
  /memex8 export <file>          Dump all memories to a JSON bundle
  /memex8 import <file>          Restore memories from a JSON bundle
                                 (<file> lives under $HERMES_HOME/plugin-data/memex8/
                                 unless enable_export_import=true)
  /memex8 realms                 List knowledge realms + counts
  /memex8 help                   Show this message

Subcommands are read-only by default except 'remember', 'forget',
'export', and 'import'. Subcommands operate on the live memex8 server.
"""


def _memex8_stats(_argv) -> str:
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized; can't read stats"
    breaker_open = provider._is_breaker_open()
    fails = provider._consecutive_failures
    threshold = provider._breaker_threshold
    cooldown = provider._breaker_cooldown
    # cooldown remaining
    if breaker_open:
        from time import monotonic
        remaining = max(0.0, provider._breaker_open_until - monotonic())
        breaker_str = f"OPEN (resets in {remaining:.0f}s)"
    else:
        breaker_str = "closed"
    out = [
        "memex8 status",
        f"  plugin version     : {_PLUGIN_VERSION}",
        f"  circuit breaker    : {breaker_str}",
        f"  consecutive fails  : {fails} / {threshold} (cooldown {cooldown}s)",
        f"  turn counter       : {provider._turn_counter}",
        f"  injected this sess : {len(provider._injected_this_session)} unique",
    ]
    return "\n".join(out)


def _memex8_status(_argv) -> str:
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    cfg = provider._config or {}
    return "\n".join([
        "memex8 config",
        f"  base_url         : {cfg.get('base_url', _DEFAULT_BASE_URL)}",
        f"  has api_key      : {bool(cfg.get('api_key'))}",
        f"  timeout          : {cfg.get('timeout', _DEFAULT_TIMEOUT)}s",
        f"  recall min score : {cfg.get('recall_min_score', _DEFAULT_RECALL_MIN_SCORE)}",
        f"  recall top_k     : {cfg.get('recall_top_k', _DEFAULT_RECALL_TOP_K)}",
        f"  dedup_injected   : {cfg.get('dedup_injected_memories', False)}",
        f"  recall_max_chars : {cfg.get('recall_max_inject_chars', 'off')}",
        f"  promote_decisions: {cfg.get('promote_decisions', False)}",
        f"  promote_evals    : {cfg.get('promote_evaluations', False)}",
        f"  sanitize         : {cfg.get('sanitize_for_prompt', False)}",
        f"  custom template  : {'yes' if cfg.get('system_prompt_template') else 'no'}",
        f"  enable export    : {cfg.get('enable_export_import', False)}",
    ])


def _memex8_search(argv) -> str:
    if len(argv) < 2:
        return "Usage: /memex8 search <query>"
    query = " ".join(argv[1:]).strip()
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        client = provider._get_client()
        # _Client.search(query, top_k=8, realm=None, min_score=0.3)
        results = client.search(query, top_k=5)
    except Exception as e:
        return f"memex8 search failed: {e}"
    if not results:
        return f"No matches for: {query!r}"
    out = [f"Top-5 search hits for: {query!r}", ""]
    for i, r in enumerate(results, 1):
        content = (r.get("content") or "").strip().replace("\n", " ")
        if len(content) > 200:
            content = content[:197] + "..."
        score = r.get("score", r.get("similarity", 0))
        mid = r.get("id") or "?"
        out.append(f"  {i}. [{mid}] score={score:.3f}")
        out.append(f"     {content}")
        out.append("")
    return "\n".join(out).rstrip()


def _memex8_recall(argv) -> str:
    if len(argv) < 2:
        return "Usage: /memex8 recall <query>  (used as a hint for filtering)"
    query = " ".join(argv[1:]).strip()
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        client = provider._get_client()
        # _Client.recall(top_k=8, realm=None) — runs a semantic search for the
        # highest-importance memories; the query is used to filter by feeding
        # it into search() instead.
        results = client.search(query, top_k=5)
    except Exception as e:
        return f"memex8 recall failed: {e}"
    if not results:
        return f"No high-importance recalls for: {query!r}"
    out = [f"High-importance recalls for: {query!r}", ""]
    for i, r in enumerate(results, 1):
        content = (r.get("content") or "").strip().replace("\n", " ")
        if len(content) > 200:
            content = content[:197] + "..."
        importance = r.get("importance", r.get("score", 0))
        mid = r.get("id") or "?"
        out.append(f"  {i}. [{mid}] importance={importance:.3f}")
        out.append(f"     {content}")
        out.append("")
    return "\n".join(out).rstrip()


def _memex8_remember(argv) -> str:
    if len(argv) < 2:
        return (
            "Usage: /memex8 remember <content> [--public]\n"
            "  --public  mark this memory as eligible for A2A sharing"
        )
    # Parse a leading --public flag without breaking on content
    # that happens to contain the word "public" mid-sentence.
    visibility = None
    rest = argv[1:]
    if rest and rest[0] == "--public":
        visibility = "public"
        rest = rest[1:]
    if not rest:
        return "Usage: /memex8 remember <content> [--public]"
    content = " ".join(rest).strip()
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        client = provider._get_client()
        result = client.store(content, source="slash-command", visibility=visibility)
    except Exception as e:
        return f"memex8 remember failed: {e}"
    mid = result.get("id") if isinstance(result, dict) else None
    suffix = " (visibility=public)" if visibility == "public" else ""
    return f"Stored memory (id={mid or '?'}){suffix}." if mid else f"Stored memory{suffix}."


def _memex8_forget(argv) -> str:
    if len(argv) < 2:
        return "Usage: /memex8 forget <memory_id>"
    mid = argv[1].strip()
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        client = provider._get_client()
        client.delete(mid)
    except Exception as e:
        return f"memex8 forget failed: {e}"
    return f"Forgot memory {mid}."


def _memex8_export(argv) -> str:
    if len(argv) < 2:
        return "Usage: /memex8 export <path>  (writes JSON bundle to path)"
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        out_path = provider.resolve_bundle_path(argv[1].strip())
        n = provider.export_memories(out_path)
    except Exception as e:
        return f"memex8 export failed: {e}"
    return f"Exported {n} memories to {out_path}"


def _memex8_import(argv) -> str:
    if len(argv) < 2:
        return "Usage: /memex8 import <path>  (reads JSON bundle from path)"
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        in_path = provider.resolve_bundle_path(argv[1].strip())
        n = provider.import_memories(in_path)
    except Exception as e:
        return f"memex8 import failed: {e}"
    return f"Imported {n} memories from {in_path}"


def _memex8_realms(_argv) -> str:
    provider = _MEMEX8_PROVIDER
    if provider is None:
        return "memex8 provider not initialized"
    try:
        client = provider._get_client()
        realms = client.realms()
    except Exception as e:
        return f"memex8 realms failed: {e}"
    if not realms:
        return "No knowledge realms found."
    out = ["Knowledge realms:"]
    for r in realms[:20]:
        if isinstance(r, dict):
            name = r.get("name") or r.get("realm") or "?"
            count = r.get("memory_count") or r.get("count") or 0
            out.append(f"  - {name} ({count} memories)")
        else:
            out.append(f"  - {r}")
    return "\n".join(out)


# Subcommand dispatch table — Feature 6
_MEMEX8_SUBCOMMANDS = {
    "stats": _memex8_stats,
    "status": _memex8_status,
    "search": _memex8_search,
    "recall": _memex8_recall,
    "remember": _memex8_remember,
    "forget": _memex8_forget,
    "export": _memex8_export,
    "import": _memex8_import,
    "realms": _memex8_realms,
}

# Module-level handle set by register(); subcommands read this at call time
_MEMEX8_PROVIDER: Optional["Memex8MemoryProvider"] = None


def _handle_memex8_slash(raw_args: str) -> Optional[str]:
    argv = raw_args.strip().split()
    if not argv or argv[0] in {"help", "-h", "--help"}:
        return _MEMEX8_HELP
    handler = _MEMEX8_SUBCOMMANDS.get(argv[0])
    if handler is None:
        return f"Unknown subcommand: {argv[0]}\n\n{_MEMEX8_HELP}"
    return handler(argv)


# ---------------------------------------------------------------------------
# Plugin Registration
# ---------------------------------------------------------------------------

def register(ctx) -> None:
    """Register memex8 as a memory provider plugin."""
    global _MEMEX8_PROVIDER
    provider = Memex8MemoryProvider()
    _MEMEX8_PROVIDER = provider
    ctx.register_memory_provider(provider)
    # Feature 6: /memex8 slash command — stats, search, recall, remember,
    # forget, export, import, realms, help.
    ctx.register_command(
        "memex8",
        handler=_handle_memex8_slash,
        description="Inspect and manage your memex8 memory store (stats, search, recall, remember, forget, export, import, realms).",
    )
