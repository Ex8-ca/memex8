"""HTTP client for the memex8 REST API."""
from __future__ import annotations

import re
from typing import Any, Optional

_DEFAULT_TIMEOUT = 10.0
_DEFAULT_SEARCH_TIMEOUT = 15.0


def _valid_receipt(value) -> bool:
    return (isinstance(value, str) and 0 < len(value) <= 256
            and re.fullmatch(r"[A-Za-z0-9_.:-]+", value) is not None)


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

    def search(self, query: str, top_k: int = 8, realm: Optional[str] = None, min_score: float = 0.3, compact: bool = False, tags: Optional[list] = None, visibility: Optional[str] = None, *, timeout: Optional[float] = None) -> dict:
        body = {"query": query, "limit": top_k, "min_score": min_score}
        if tags is not None:
            body["tags"] = tags
        if visibility is not None:
            body["visibility"] = visibility
        if compact:
            body["compact"] = True
        if realm:
            body["realm"] = realm
        return self.request("POST", "/api/v1/memories/search", json_body=body, timeout=_DEFAULT_SEARCH_TIMEOUT if timeout is None else timeout)

    def recall(self, top_k: int = 8, realm: str = None, compact: bool = False) -> dict:
        params = {"limit": top_k}
        if compact:
            params["compact"] = "true"
        if realm:
            params["realm"] = realm
        return self.request("GET", "/api/v1/memories/recall", params=params)

    def store(
        self,
        content: str,
        realm_hint: str = None,
        tags: list = None,
        source: str = None,
        memory_type: str = None,
        visibility: str = None,
        certified: bool = False,
    ) -> dict:
        """Store a memory. Pass ``visibility="public"`` to make it
        eligible for sharing via the Hermes-A2A bridge; the default
        ``None`` is treated as ``"private"`` by the server so a
        forgetful caller can never accidentally mark a memory
        shareable.
        """
        if not isinstance(certified, bool):
            raise ValueError("certified must be a boolean")
        body = {"content": content}
        if certified:
            body["certified"] = True
        if realm_hint:
            body["realm_hint"] = realm_hint
        if tags:
            body["tags"] = tags
        if source:
            body["source"] = source
        if memory_type:
            body["memory_type"] = memory_type
        if visibility:
            # Server-side normalize_visibility() fail-closes to
            # "private" on any value it doesn't recognize, so passing
            # garbage here is safe.
            body["visibility"] = visibility
        return self.request("POST", "/api/v1/memories", json_body=body)

    def timeline(self, **params) -> dict:
        return self.request("GET", "/api/v1/memories/timeline",
                            params={k: v for k, v in params.items() if v is not None})

    def batch_get(self, ids: list) -> dict:
        return self.request("POST", "/api/v1/memories/batch-get", json_body={"ids": ids})

    def restore_duplicate(self, operation_id: str) -> dict:
        return self.request("POST", f"/api/v1/dedup/{operation_id}/restore")

    def unconsolidate(self, memory_id: str) -> dict:
        return self.request("POST", f"/api/v1/memories/{memory_id}/unconsolidate")

    def certify(self, memory_id: str, revoke: bool = False, by: str = None, note: str = None) -> dict:
        if not isinstance(revoke, bool):
            raise ValueError("revoke must be a boolean")
        if revoke:
            return self.request("DELETE", f"/api/v1/memories/{memory_id}/certify")
        body = {}
        if by is not None:
            body["by"] = by
        if note is not None:
            body["note"] = note
        return self.request("POST", f"/api/v1/memories/{memory_id}/certify", json_body=body)

    def delete(self, memory_id: str) -> dict:
        return self.request("DELETE", f"/api/v1/memories/{memory_id}")

    # ---- dreams (a dream is not a memory; these are separate endpoints) ----

    def dreams_pending(self) -> dict:
        """The newest pending dream in full, plus how many are waiting."""
        try:
            listing = self.request("GET", "/api/v1/dreams", params={"status": "pending", "limit": 5})
        except RuntimeError as e:
            if "(404)" in str(e):
                return {"error": "This memex8 server has no dreams API; upgrade memex8 to a version with dreams."}
            raise
        dreams = listing.get("dreams") or []
        if not dreams:
            return {"pending": 0, "dream": None}
        full = self.request("GET", f"/api/v1/dreams/{dreams[0]['id']}")
        return {"pending": listing.get("pending", len(dreams)), "dream": full}

    def dream_keep(self, dream_id: str, remember: bool = False) -> dict:
        if not isinstance(remember, bool):
            raise ValueError("remember must be a boolean")
        return self.request("POST", f"/api/v1/dreams/{dream_id}/keep", json_body={"remember": remember})

    def dream_discard(self, dream_id: str) -> dict:
        return self.request("DELETE", f"/api/v1/dreams/{dream_id}")

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
        tags: Optional[List[str]] = None, memory_type: Optional[str] = None,
    ) -> Optional[str]:
        """Send conversation summary via webhook. Returns the archive id if the
        server returns one, else None. Returns False-y on transport failure.

        The bool→id change is intentional: pre-compress checkpoint API v2 wants
        a referent it can put into the summary prompt so a later recall can
        cite the checkpoint. ``tags`` and ``memory_type`` are forwarded when
        set; callers that omit them keep the server's historical defaults
        (``conversation``/``platform:<platform>``, memory_type ``general``).
        """
        try:
            body: Dict[str, Any] = {"summary": summary, "source": source, "platform": platform}
            if tags:
                body["tags"] = tags
            if memory_type:
                body["memory_type"] = memory_type
            resp_data = self.request(
                "POST", "/api/v1/webhooks/conversation",
                json_body=body,
                timeout=15.0,
            )
            # A generic acknowledgement is not a retrievable durable receipt.
            if isinstance(resp_data, dict):
                for key in ("id", "archive_id", "checkpoint_id", "uuid"):
                    if _valid_receipt(resp_data.get(key)):
                        return resp_data[key]
            return None
        except Exception:
            return None

    def list_memories(
        self,
        *,
        limit: int = 100,
        offset: int = 0,
        realm: Optional[str] = None,
    ) -> Dict[str, Any]:
        """GET /api/v1/memories — paginated list of memories.

        Feature 5. Returns the raw server response (dict with ``memories``,
        ``total``, ``limit``, ``offset``). Each memory has the server's
        canonical shape; callers should read fields defensively. Used by
        export_memories() to dump all memories to JSON in pages of 100.
        """
        params = {"limit": str(limit), "offset": str(offset)}
        if realm:
            params["realm"] = realm
        return self.request("GET", "/api/v1/memories", params=params)
