#!/usr/bin/env python3
"""Truncate memex8.md digest to keep only the most recent 30 entries."""
import os

path = "/home/marc/.memex8/memex8.md"
with open(path) as f:
    content = f.read()

# Find the header divider
# Format is: header\n---\n## 2026-...\n...\n---\n## 2026-...\n...
idx = content.find("\n---\n##")
if idx == -1:
    print("No entries found")
    exit(1)

# Header is everything before the first "---\n##"
# Find the actual header end
header_end = content.find("\n---\n", 0, idx + 20)
header = content[:header_end+5]
entries_part = content[header_end+5:]

# Split on "\n---\n##" pattern
entries = entries_part.split("\n---\n")
entries = [e for e in entries if e.strip().startswith("##")]
print(f"Current entries: {len(entries)}")

# Dedup: skip entries with same date + same scanned count
unique = []
seen = set()
for e in entries:
    lines = e.strip().split("\n")
    date = next((l for l in lines if l.startswith("## 20")), "")
    scanned = next((l for l in lines if "Scanned" in l), "")
    key = (date, scanned)
    if key not in seen:
        seen.add(key)
        unique.append(e)

print(f"After dedup: {len(unique)}")

# Keep last 30
max_entries = 30
if len(unique) > max_entries:
    unique = unique[-max_entries:]
    print(f"Truncated to: {len(unique)}")

# Reassemble with "\n---\n" separator
new_content = header + "\n---\n" + "\n---\n".join(unique)

# Write
with open(path, "w") as f:
    f.write(new_content)

new_size = os.path.getsize(path)
print(f"New size: {new_size/1024:.0f} KB ({len(unique)} entries)")
