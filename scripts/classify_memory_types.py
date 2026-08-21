#!/usr/bin/env python3
"""
Retroactively classify memory_type for all existing memories in memex8.

Two-pass strategy:
1. Keyword classifier (fast, deterministic) — matches obvious signals.
2. LLM fallback (slow, smart) — for memories the keyword pass couldn't classify.

High confidence threshold: if neither pass is confident, leave as "general".

Usage:
    python3 scripts/classify_memory_types.py [--dry-run] [--batch-size 50]
"""

import argparse
import json
import os
import re
import sys
import time
from typing import Optional

import httpx

MEMEX8_URL = os.environ.get("MEMEX8_URL", "http://localhost:8080")
MEMEX8_API_KEY = os.environ.get("MEMEX8_API_KEY", "")
LLM_API_URL = os.environ.get("LLM_API_URL", "")
LLM_MODEL = os.environ.get("LLM_MODEL", "")
LLM_API_KEY = os.environ.get("LLM_API_KEY", os.environ.get("OPENAI_API_KEY", ""))

VALID_TYPES = {
    "profile", "preference", "relationship", "learning",
    "fact", "entity", "setup", "pattern", "context", "observation", "artifact",
    "project", "goal", "decision", "commitment",
    "event", "instruction", "error", "issue", "request",
    "general", "session_summary", "manual", "consolidated",
}

# Keyword patterns — each tuple is (memory_type, regex)
KEYWORD_RULES = [
    # Preferences
    ("preference", re.compile(
        r"\b(like|likes|liked|prefer|prefers|preferred|favorite|favourite|love|loves|hate|hates|enjoy|enjoys)\b",
        re.IGNORECASE)),
    # Decisions
    ("decision", re.compile(
        r"\b(decided|decision|chose to|going with|will use|settled on)\b",
        re.IGNORECASE)),
    # Goals/commitments
    ("goal", re.compile(
        r"\b(goal|objective|target|aim|plan to|intend to|want to)\b",
        re.IGNORECASE)),
    # Errors / issues
    ("error", re.compile(
        r"\b(error|bug|fail|failure|broken|crash|panic)\b",
        re.IGNORECASE)),
    # Instructions / procedural
    ("instruction", re.compile(
        r"\b(how to|step \d+|install|configure|setup|deploy|run the)\b",
        re.IGNORECASE)),
    # Events (ISO dates)
    ("event", re.compile(
        r"\b\d{4}-\d{2}-\d{2}\b|\b(yesterday|today|last week|last month)\b",
        re.IGNORECASE)),
    # People (relationship)
    ("relationship", re.compile(
        r"\b(wife|husband|friend|coworker|colleague|brother|sister|mother|father|son|daughter|partner)\b",
        re.IGNORECASE)),
    # Profile facts (about the user themselves)
    ("profile", re.compile(
        r"\b(my name|i am|i'm|i live|i work|i have|my wife|my husband|my home|my job)\b",
        re.IGNORECASE)),
    # Setup facts (configuration, infra)
    ("setup", re.compile(
        r"\b(port|host|url|endpoint|env var|config|crontab|docker|kubernetes)\b",
        re.IGNORECASE)),
]


def keyword_classify(content: str, heading: Optional[str] = None) -> Optional[str]:
    """Returns the highest-confidence type, or None if no keyword matches."""
    text = (heading or "") + " " + content
    matches = {}
    for mtype, pattern in KEYWORD_RULES:
        if pattern.search(text):
            matches[mtype] = matches.get(mtype, 0) + 1

    if not matches:
        return None

    # Priority order when multiple types match
    priority = ["profile", "preference", "decision", "relationship",
                "error", "instruction", "goal", "event", "setup"]
    for p in priority:
        if p in matches:
            return p
    return max(matches, key=matches.get)


def llm_classify(content: str, heading: Optional[str] = None,
                 client: Optional[httpx.Client] = None) -> Optional[str]:
    """Ask the LLM to classify. Returns the type or None on failure/low confidence."""
    if not LLM_API_URL or not client:
        return None

    heading_prefix = ((heading or "") + "\n") if heading else ""
    prompt = f"""Classify this memory into ONE of these types:
profile, preference, relationship, learning, fact, entity, setup, pattern,
context, observation, artifact, project, goal, decision, commitment,
event, instruction, error, issue, request, general

Reply with ONLY the type name, nothing else. If uncertain, reply: general

Memory:
{heading_prefix}{content[:500]}"""

    try:
        headers = {}
        if LLM_API_KEY:
            headers["Authorization"] = f"Bearer {LLM_API_KEY}"
        resp = client.post(
            f"{LLM_API_URL}/v1/chat/completions",
            headers=headers,
            json={
                "model": LLM_MODEL,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": 10,
                "temperature": 0.0,
            },
            timeout=30.0,
        )
        resp.raise_for_status()
        data = resp.json()
        answer = data["choices"][0]["message"]["content"].strip().lower()
        # Strip quotes, punctuation
        answer = answer.strip(".,\"'`")
        if answer in VALID_TYPES:
            return answer
        return None
    except Exception as e:
        print(f"  LLM error: {e}", file=sys.stderr)
        return None


def list_all_memories() -> list[dict]:
    """Page through all memories via REST API."""
    all_mems = []
    offset = 0
    page_size = 100
    while True:
        resp = httpx.get(
            f"{MEMEX8_URL}/api/v1/memories",
            headers={"Authorization": f"Bearer {MEMEX8_API_KEY}"},
            params={"sort": "ingested_at", "direction": "asc",
                    "limit": page_size, "offset": offset},
            timeout=60.0,
        )
        resp.raise_for_status()
        data = resp.json()
        # API returns either {memories: [...]} or [...] depending on shape
        batch = data if isinstance(data, list) else data.get("memories", data.get("results", []))
        if not batch:
            break
        all_mems.extend(batch)
        if len(batch) < page_size:
            break
        offset += page_size
    return all_mems


def update_memory_type(memory_id: str, new_type: str,
                       dry_run: bool = False) -> bool:
    """PATCH the memory's memory_type field."""
    if dry_run:
        return True
    # Try PATCH /api/v1/memories/{id} first
    resp = httpx.patch(
        f"{MEMEX8_URL}/api/v1/memories/{memory_id}",
        headers={"Authorization": f"Bearer {MEMEX8_API_KEY}",
                 "Content-Type": "application/json"},
        json={"memory_type": new_type},
        timeout=30.0,
    )
    if resp.status_code in (200, 204):
        return True
    # Fall back to POST if PATCH isn't supported
    if resp.status_code == 405:
        resp = httpx.post(
            f"{MEMEX8_URL}/api/v1/memories/{memory_id}",
            headers={"Authorization": f"Bearer {MEMEX8_API_KEY}",
                     "Content-Type": "application/json"},
            json={"memory_type": new_type},
            timeout=30.0,
        )
        return resp.status_code in (200, 204)
    print(f"  Update failed for {memory_id}: HTTP {resp.status_code} {resp.text[:100]}", file=sys.stderr)
    return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true",
                        help="Show what would change without writing")
    parser.add_argument("--batch-size", type=int, default=50,
                        help="Memories per LLM batch (controls pacing)")
    parser.add_argument("--skip-existing", action="store_true",
                        help="Skip memories that already have a non-general type")
    args = parser.parse_args()

    if not MEMEX8_API_KEY:
        print("ERROR: MEMEX8_API_KEY not set", file=sys.stderr)
        sys.exit(1)

    print(f"Fetching all memories from {MEMEX8_URL}...")
    memories = list_all_memories()
    print(f"Found {len(memories)} memories")

    stats = {"keyword": 0, "llm": 0, "unchanged": 0, "failed": 0,
             "skipped_consolidated": 0, "skipped_existing": 0}

    client = httpx.Client() if LLM_API_URL else None
    if not LLM_API_URL:
        print("(LLM_API_URL not set — running keyword-only mode)")
    else:
        print(f"(LLM fallback enabled: {LLM_MODEL} @ {LLM_API_URL})")

    for i, mem in enumerate(memories):
        mem_id = mem.get("id", "")
        content = mem.get("content", "")
        heading = mem.get("heading")
        current_type = mem.get("memory_type", "general") or "general"
        chunk_type = mem.get("chunk_type", "")

        # Skip consolidated slumber summaries — they abstract multiple memories
        if chunk_type == "consolidated":
            stats["skipped_consolidated"] += 1
            continue

        # Skip if already typed and --skip-existing
        if args.skip_existing and current_type not in ("general", "", "session_summary", "manual"):
            stats["skipped_existing"] += 1
            continue

        # Pass 1: keyword
        new_type = keyword_classify(content, heading)
        source = "keyword"

        # Pass 2: LLM fallback if keyword missed
        if new_type is None and client is not None:
            new_type = llm_classify(content, heading, client)
            source = "llm"

        # High confidence: only update if we got a real type and it's different
        if new_type is None or new_type == current_type:
            stats["unchanged"] += 1
            continue

        # Update
        ok = update_memory_type(mem_id, new_type, dry_run=args.dry_run)
        if ok:
            if source == "keyword":
                stats["keyword"] += 1
            else:
                stats["llm"] += 1
            preview = (heading or content)[:60].replace("\n", " ")
            print(f"  [{i+1}/{len(memories)}] {mem_id[:8]}... "
                  f"'{current_type}' → '{new_type}' ({source}) — '{preview}'")
        else:
            stats["failed"] += 1

        # Pacing to avoid hammering LLM
        if source == "llm" and not args.dry_run:
            time.sleep(0.1)

    if client is not None:
        client.close()

    print("\n=== Summary ===")
    for k, v in stats.items():
        print(f"  {k}: {v}")


if __name__ == "__main__":
    main()
