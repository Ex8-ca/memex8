#!/usr/bin/env python3
"""
Cleanup script for memex8 Qdrant realm corruption.

Fixes:
1. Truncates the 4.2MB memex8.md digest to keep only the most recent 30 entries
2. Identifies corrupted realm names (containing -a/-b chains longer than 3)
3. Reports what would be cleaned (dry-run by default, use --apply to actually clean)

Usage:
    python3 ~/memex8/scripts/cleanup_realms.py        # dry-run
    python3 ~/memex8/scripts/cleanup_realms.py --apply  # apply fixes
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime
from pathlib import Path

# Qdrant client
try:
    from qdrant_client import QdrantClient
    from qdrant_client.models import Filter, FieldCondition, MatchValue, PayloadSchemaType
except ImportError:
    print("ERROR: qdrant-client not installed. Run: pip install qdrant-client")
    sys.exit(1)

QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
COLLECTION_NAME = os.environ.get("QDRANT_COLLECTION", "memories")
DIGEST_PATH = os.path.expanduser("~/.memex8/memex8.md")
MAX_DIGEST_ENTRIES = 30
MAX_REALM_NAME_LEN = 80
MAX_SPLIT_DEPTH = 3


def get_client():
    return QdrantClient(url=QDRANT_URL)


def split_depth(name: str) -> int:
    """Count the number of -a/-b suffixes in a realm name."""
    parts = name.split("-")
    return sum(1 for p in parts if p in ("a", "b"))


def is_corrupted(name: str) -> bool:
    """Check if a realm name appears corrupted."""
    # Too many -a/-b suffixes
    if split_depth(name) > MAX_SPLIT_DEPTH:
        return True
    # Name too long
    if len(name) > MAX_REALM_NAME_LEN:
        return True
    # Contains non-printable characters
    if not all(c.isprintable() or c in "\n\r\t" for c in name):
        return True
    # Looks like a device path leaked into the name
    if re.match(r"^/dev/", name):
        return True
    return False


def truncate_digest(dry_run=True):
    """Truncate memex8.md to keep only the most recent MAX_DIGEST_ENTRIES entries."""
    path = Path(DIGEST_PATH)
    if not path.exists():
        print(f"  Digest file not found: {DIGEST_PATH}")
        return

    content = path.read_text()
    if not content:
        print("  Digest file is empty.")
        return

    file_size_mb = path.stat().st_size / (1024 * 1024)
    print(f"\n=== Digest File ===")
    print(f"  Path: {DIGEST_PATH}")
    print(f"  Size: {file_size_mb:.1f} MB ({path.stat().st_size:,} bytes)")
    print(f"  Lines: {len(content.splitlines()):,}")

    # Find the header
    header_end = content.find("\n---\n\n")
    if header_end == -1:
        print("  ERROR: No header divider found in digest file.")
        return

    header = content[:header_end + 5]
    entries_part = content[header_end + 5:]

    # Split entries
    entries = [e for e in entries_part.split("\n---\n\n") if e.strip()]
    print(f"  Current entries: {len(entries)}")
    print(f"  Target entries: {MAX_DIGEST_ENTRIES}")

    if len(entries) <= MAX_DIGEST_ENTRIES:
        print("  Already within limit. No truncation needed.")
        return

    # Keep only the most recent entries (last MAX_DIGEST_ENTRIES)
    keep = entries[-MAX_DIGEST_ENTRIES:]

    # Reassemble
    new_content = header + "\n---\n\n".join(keep)
    new_size_kb = len(new_content) / 1024

    print(f"  New size: {new_size_kb:.0f} KB (reduction: {file_size_mb:.1f} MB -> {new_size_kb/1024:.1f} MB)")

    if dry_run:
        print("  [DRY RUN] Would truncate. Use --apply to actually do it.")
    else:
        # Backup first
        backup = path.with_suffix(".md.bak")
        path.rename(backup)
        print(f"  Backed up to {backup}")
        path.write_text(new_content)
        print(f"  Truncated to {len(keep)} entries ({new_size_kb:.0f} KB).")


def list_corrupted_realms(client, dry_run=True):
    """Find all corrupted realm names."""
    print(f"\n=== Realm Cleanup ===")
    print(f"  Qdrant: {QDRANT_URL}")
    print(f"  Collection: {COLLECTION_NAME}")

    # Get all unique realm names
    # Scroll all points and collect realm_name payloads
    offset = None
    realm_names = {}  # realm_name -> count
    total = 0

    while True:
        result = client.scroll(
            collection_name=COLLECTION_NAME,
            limit=1000,
            with_payload=True,
            with_vectors=False,
            offset=offset,
        )
        points, next_offset = result
        if not points:
            break

        for point in points:
            total += 1
            payload = point.payload or {}
            realm_name = payload.get("realm_name", "unknown")
            realm_names[realm_name] = realm_names.get(realm_name, 0) + 1

        if next_offset is None:
            break
        offset = next_offset

    print(f"  Total memories: {total}")
    print(f"  Unique realm names: {len(realm_names)}")

    corrupted = []
    for name, count in sorted(realm_names.items(), key=lambda x: -x[1]):
        if is_corrupted(name):
            depth = split_depth(name)
            corrupted.append((name, count, depth))

    if not corrupted:
        print("  No corrupted realm names found.")
        return

    print(f"\n  Found {len(corrupted)} corrupted realm names:")
    for name, count, depth in corrupted:
        display_name = name[:60] + "..." if len(name) > 60 else name
        print(f"    [{count:4d}] depth={depth:2d}  {display_name}")

    if dry_run:
        print(f"\n  [DRY RUN] Found {len(corrupted)} corrupted realms. Use --apply to clean.")
    else:
        print(f"\n  Applying cleanup...")
        _apply_realm_cleanup(client, corrupted)


def _apply_realm_cleanup(client, corrupted):
    """Rename corrupted realms by stripping excess -a/-b suffixes."""
    for name, count, depth in corrupted:
        # Find the base name by stripping -a/-b suffixes
        base = name
        while True:
            stripped = re.sub(r"-[ab]$", "", base)
            if stripped == base:
                break
            base = stripped

        # If the base is still too long, truncate it
        if len(base) > MAX_REALM_NAME_LEN:
            base = base[:MAX_REAL_NAME_LEN].rsplit(" ", 1)[0] or base[:MAX_REAL_NAME_LEN]

        if not base:
            base = "general"

        print(f"    Renaming: '{name[:50]}...' ({count} memories) -> '{base}'")

        # Update all memories with this realm_name
        # We need to filter by realm_name and update the payload
        # Use the Qdrant set_payload API
        try:
            # Find all points with this realm_name
            offset = None
            while True:
                result = client.scroll(
                    collection_name=COLLECTION_NAME,
                    scroll_filter=Filter(
                        must=[
                            FieldCondition(
                                key="realm_name", match=MatchValue(value=name)
                            )
                        ]
                    ),
                    limit=100,
                    with_payload=True,
                    with_vectors=False,
                    offset=offset,
                )
                points, next_offset = result
                if not points:
                    break

                point_ids = [p.id for p in points]
                if point_ids:
                    client.set_payload(
                        collection_name=COLLECTION_NAME,
                        payload={"realm_name": base},
                        points=point_ids,
                    )

                if next_offset is None:
                    break
                offset = next_offset

        except Exception as e:
            print(f"      ERROR updating '{name[:40]}': {e}")

    print(f"  Done. {len(corrupted)} realm names cleaned.")


def main():
    parser = argparse.ArgumentParser(description="Cleanup memex8 corrupted realms and digest")
    parser.add_argument("--apply", action="store_true", help="Apply fixes (default: dry-run)")
    args = parser.parse_args()

    dry_run = not args.apply
    print(f"{'=== DRY RUN ===' if dry_run else '=== APPLYING FIXES ==='}")
    print(f"Date: {datetime.now().isoformat()}")

    # Step 1: Truncate digest
    truncate_digest(dry_run=dry_run)

    # Step 2: Find and fix corrupted realm names
    try:
        client = get_client()
        list_corrupted_realms(client, dry_run=dry_run)
    except Exception as e:
        print(f"\n  ERROR connecting to Qdrant: {e}")
        print("  Make sure Qdrant is running at {}".format(QDRANT_URL))
        sys.exit(1)


if __name__ == "__main__":
    main()
