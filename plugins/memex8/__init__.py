"""memex8 memory plugin — MemoryProvider interface for Hermes-Agent.

Self-hosted memory via memex8 REST API running in Docker.

Features:
- Semantic search across all memories
- Store conversation context and facts
- Auto-assign realms based on cosine similarity
- TurboQuant compression for efficient storage
- Background slumber mode for memory maintenance

Config (env vars):
  MEMEX8_API_KEY   — API key for authentication
  MEMEX8_BASE_URL  — API endpoint (default: http://localhost:8080)
"""

from __future__ import annotations

import json
import logging
import os
import re
import threading
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

_DEFAULT_BASE_URL = "http://localhost:8080"


# ---------------------------------------------------------------------------
# Tool schemas
# ---------------------------------------------------------------------------

SEARCH_SCHEMA = {
    "name": "memex8_search",
    "description": "Semantic search across all memex8 memories. Use before making decisions to check existing project knowledge.",
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
    "description": "Get high-importance memories — a summary of what matters most. Good for session startup context.",
    "parameters": {
        "type": "object",
        "properties": {
            "top_k": {"type": "integer", "description": "Max results (default: 8, max: 20)."},
            "realm": {"type": "string", "description": "Filter to a specific realm (optional)."},
        },
        "required": [],
    },
}

REMEMBER_SCHEMA = {
    "name": "memex8_remember",
    "description": "Store a fact, decision, or observation for future sessions. This memory will be searchable and may influence future context.",
    "parameters": {
        "type": "object",
        "properties": {
            "content": {"type": "string", "description": "The fact, decision, or observation to remember."},
            "realm_hint": {"type": "string", "description": "Suggested realm name (e.g. 'project-termex8', 'user-preferences'). Auto-assigned if omitted."},
        },
        "required": ["content"],
    },
}

FORGET_SCHEMA = {
    "name": "memex8_forget",
    "description": "Delete a memory by ID. Use only when the user explicitly asks to forget something.",
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
    "description": "List all knowledge realms with memory counts. Shows how memories are organized.",
    "parameters": {
        "type": "object",
        "properties": {},
        "required": [],
    },
}


# ---------------------------------------------------------------------------
# HTTP client
# ---------------------------------------------------------------------------

class _Client:
    """HTTP client for the memex8 REST API."""

    def __init__(self, api_key: str, base_url: str):
        self.api_key = api_key
        self.base_url = re.sub(r"/+$", "", base_url)

    def _headers(self) -> dict:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

    def request(self, method: str, path: str, *, params=None, json_body=None, timeout: float = 10.0) -> Any:
        import requests
        url = f"{self.base_url}{path}"
        resp = requests.request(
            method.upper(), url,
            params=params,
            json=json_body if method.upper() not in {"GET", "DELETE"} else None,
            headers=self._headers(),
            timeout=timeout,
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
        return self.request("POST", "/api/v1/memories/search", json_body=body, timeout=15.0)

    def recall(self, top_k: int = 8, realm: str = None) -> dict:
        params = {"limit": top_k}
        if realm:
            params["realm"] = realm
        return self.request("GET", "/api/v1/memories/recall", params=params)

    def store(self, content: str, realm_hint: str = None, tags: list = None) -> dict:
        body = {"content": content}
        if realm_hint:
            body["realm_hint"] = realm_hint
        if tags:
            body["tags"] = tags
        return self.request("POST", "/api/v1/memories", json_body=body)

    def delete(self, memory_id: str) -> dict:
        return self.request("DELETE", f"/api/v1/memories/{memory_id}")

    def realms(self) -> dict:
        return self.request("GET", "/api/v1/realms")

    def get_memory(self, memory_id: str) -> dict:
        return self.request("GET", f"/api/v1/memories/{memory_id}")

    def upvote(self, memory_id: str) -> dict:
        return self.request("POST", f"/api/v1/memories/{memory_id}/upvote")

    def stats(self) -> dict:
        return self.request("GET", "/api/v1/stats")

    def health(self) -> str:
        import requests
        url = f"{self.base_url}/health"
        resp = requests.get(url, timeout=5.0)
        return resp.text if resp.ok else ""


# ---------------------------------------------------------------------------
# Overlay formatter — builds context block for system prompt
# ---------------------------------------------------------------------------

def _build_context_block(search_results: list, recall_results: list = None) -> str:
    """Format search/recall results as a context block for the system prompt."""
    lines: list[str] = []

    if recall_results:
        lines.append("[memex8 — Important Context]")
        for r in recall_results[:5]:
            content = (r.get("content") or "").strip()
            if len(content) > 200:
                content = content[:197] + "..."
            realm = r.get("realm_name", r.get("realm", ""))
            lines.append(f"- [{realm}] {content}")
        lines.append("")

    if search_results:
        lines.append("[memex8 — Search Results]")
        for r in search_results[:5]:
            content = (r.get("content") or "").strip()
            if len(content) > 200:
                content = content[:197] + "..."
            realm = r.get("realm_name", r.get("realm", ""))
            score = r.get("score", r.get("importance", 0))
            lines.append(f"- [{realm}] (score: {score:.2f}) {content}")
        lines.append("")

    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main plugin class
# ---------------------------------------------------------------------------

class Memex8MemoryProvider:
    """memex8 self-hosted memory — MemoryProvider interface for Hermes-Agent."""

    def __init__(self):
        self._client: _Client | None = None
        self._session_id = ""
        self._user_id = "default"
        self._lock = threading.Lock()

        # Prefetch caches
        self._recall_result: list = []
        self._search_results: list = []

    # ── Core identity ──────────────────────────────────────────────────────

    @property
    def name(self) -> str:
        return "memex8"

    def is_available(self) -> bool:
        return bool(os.environ.get("MEMEX8_API_KEY"))

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {"key": "api_key", "description": "memex8 API key", "secret": True, "required": True, "env_var": "MEMEX8_API_KEY"},
            {"key": "base_url", "description": "memex8 REST API endpoint", "default": _DEFAULT_BASE_URL, "env_var": "MEMEX8_BASE_URL"},
        ]

    # ── Lifecycle ──────────────────────────────────────────────────────────

    def initialize(self, session_id: str, **kwargs) -> None:
        api_key = os.environ.get("MEMEX8_API_KEY", "")
        base_url = os.environ.get("MEMEX8_BASE_URL", _DEFAULT_BASE_URL)
        self._client = _Client(api_key, base_url)
        self._session_id = session_id
        self._user_id = kwargs.get("user_id", "default") or "default"
        logger.info("memex8 initialized: %s", base_url)

    def system_prompt_block(self) -> str:
        return (
            "# memex8 Memory\n"
            "Active. Self-hosted memory with Qdrant vector storage and TurboQuant compression.\n"
            "Use memex8_recall for important context at session start.\n"
            "Use memex8_search to find relevant memories.\n"
            "Use memex8_remember to store facts for future sessions.\n"
            "Use memex8_realms to see how memories are organized.\n"
        )

    # ── Background prefetch ───────────────────────────────────────────────

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        """Prefetch recall results in background for next turn."""
        if not self._client:
            return
        threading.Thread(
            target=self._prefetch_recall,
            args=(query,),
            name="memex8-prefetch",
            daemon=True,
        ).start()

    def _prefetch_recall(self, query: str) -> None:
        try:
            results = self._client.recall(top_k=8)
            with self._lock:
                self._recall_result = results.get("results", results) if isinstance(results, dict) else []
        except Exception as exc:
            logger.debug("memex8 prefetch failed: %s", exc)

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        """Return prefetched context block."""
        with self._lock:
            recall = self._recall_result
            self._recall_result = []
        return _build_context_block([], recall) if recall else ""

    # ── Turn sync ──────────────────────────────────────────────────────────

    def sync_turn(self, user_content: str, assistant_content: str, *, session_id: str = "") -> None:
        """Auto-store conversation turns as memories."""
        if not self._client or not user_content:
            return
        try:
            # Only store if there's meaningful content
            if len(user_content.strip()) > 20:
                content = f"## User\n\n{user_content.strip()}\n\n## Assistant\n\n{assistant_content.strip()}"
                self._client.store(
                    content,
                    realm_hint="conversation",
                    tags=["conversation", "auto-stored"],
                )
        except Exception as exc:
            logger.debug("memex8 sync_turn failed: %s", exc)

    # ── Tools ──────────────────────────────────────────────────────────────

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        return [SEARCH_SCHEMA, RECALL_SCHEMA, REMEMBER_SCHEMA, FORGET_SCHEMA, REALMS_SCHEMA]

    def handle_tool_call(self, tool_name: str, args: dict, **kwargs) -> str:
        from tools.registry import tool_error
        if not self._client:
            return tool_error("memex8 not initialized")
        try:
            return json.dumps(self._dispatch(tool_name, args))
        except Exception as exc:
            return tool_error(str(exc))

    def _dispatch(self, tool_name: str, args: dict) -> Any:
        c = self._client

        if tool_name == "memex8_search":
            query = args.get("query", "")
            if not query:
                return {"error": "query is required"}
            results = c.search(
                query,
                top_k=min(int(args.get("top_k", 8)), 20),
                realm=args.get("realm"),
                min_score=float(args.get("min_score", 0.3)),
            )
            return results.get("results", results) if isinstance(results, dict) else results

        if tool_name == "memex8_recall":
            results = c.recall(
                top_k=min(int(args.get("top_k", 8)), 20),
                realm=args.get("realm"),
            )
            return results.get("results", results) if isinstance(results, dict) else results

        if tool_name == "memex8_remember":
            content = args.get("content", "")
            if not content:
                return {"error": "content is required"}
            return c.store(content, realm_hint=args.get("realm_hint"))

        if tool_name == "memex8_forget":
            memory_id = args.get("memory_id", "")
            if not memory_id:
                return {"error": "memory_id is required"}
            return c.delete(memory_id)

        if tool_name == "memex8_realms":
            return c.realms()

        return {"error": f"Unknown tool: {tool_name}"}

    # ── Optional hooks ─────────────────────────────────────────────────────

    def on_memory_write(self, action: str, target: str, content: str) -> None:
        """Mirror built-in memory writes to memex8."""
        if action != "add" or not content or not self._client:
            return
        try:
            memory_type = "preference" if target == "user" else "factual"
            self._client.store(
                content,
                realm_hint=f"user-{target}",
                tags=[memory_type, "mirrored"],
            )
        except Exception as exc:
            logger.debug("memex8 memory mirror failed: %s", exc)

    def shutdown(self) -> None:
        pass


def register(ctx) -> None:
    """Register memex8 as a memory provider plugin."""
    ctx.register_memory_provider(Memex8MemoryProvider())
