#!/usr/bin/env python3
"""Purge stored cron-job transcripts from memex8, keeping real findings.

Old plugin versions stored every scheduled cron run as a memory
("## User\\n\\n[IMPORTANT: You are running as a scheduled cron job ..." and
"# Conversation Summary\\n\\nUser: [IMPORTANT: ..."). This finds them, and:

  * silent / failure / empty runs  -> delete
  * runs with real output          -> re-store just the output as a
                                      `cron-finding` memory, then delete the
                                      transcript (with --keep-findings, default)

Dry run by default. Pass --apply to change anything.
Runs the HTTP calls inside the memex8 container so the API key never leaves it.
"""
import argparse
import json
import re
import subprocess
import sys

CONTAINER = "memex8-memex8-1"
HINT = re.compile(r"\[IMPORTANT:\s*You are running as a scheduled cron job", re.I)
SILENCE = {"[SILENT]", "SILENT", "NO_REPLY", "NO REPLY"}


def api(method, path, body=None):
    cmd = ['curl', '-sS', '-X', method, '-H', 'Authorization: Bearer $MEMEX8_API_KEY',
           '-H', 'Content-Type: application/json', f'http://localhost:8080{path}']
    sh = " ".join(f'"{c}"' if c.startswith('Authorization') else f"'{c}'" for c in cmd)
    if body is not None:
        sh += " --data-binary @-"
    out = subprocess.run(["docker", "exec", "-i", CONTAINER, "sh", "-c", sh],
                         input=json.dumps(body) if body is not None else None,
                         capture_output=True, text=True, check=True).stdout
    return json.loads(out) if out.strip() else {}


def assistant_text(content):
    """Pull the assistant part out of either stored shape."""
    if content.startswith("## User"):
        _, _, rest = content.partition("## Assistant")
        return rest.strip()
    parts = re.split(r"^Assistant:\s?", content, flags=re.M)
    return "\n".join(p.strip() for p in parts[1:]).strip()


def finding(text):
    text = (text or "").strip()
    if not text:
        return None
    lines = [l.strip() for l in text.splitlines() if l.strip()]
    if text.upper().startswith("[SILENT]"):
        return None
    if any(c.upper() in SILENCE for c in (text, lines[0], lines[-1])):
        return None
    if lines[0] == "[CRON_FAILURE]":
        return None
    return text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--drop-findings", action="store_true",
                    help="delete transcripts with real output too, without re-storing")
    a = ap.parse_args()

    memories, offset = {}, 0
    while True:
        page = api("GET", f"/api/v1/memories?limit=500&offset={offset}").get("memories", [])
        for m in page:
            memories[m["id"]] = m
        if len(page) < 500:
            break
        offset += 500
    cron = [m for m in memories.values() if HINT.search(m["content"][:600])]
    keep = {}
    for m in cron:
        f = finding(assistant_text(m["content"]))
        if f:
            keep.setdefault(f[:2000], m)
    print(f"total={len(memories)} cron_transcripts={len(cron)} "
          f"unique_findings={len(keep)} silent_or_noise={len(cron) - sum(1 for m in cron if finding(assistant_text(m['content'])))}")
    for f in list(keep)[:5]:
        print("  finding:", f[:110].replace("\n", " "))
    if not a.apply:
        print("dry run — pass --apply to delete")
        return 0

    stored = 0
    if not a.drop_findings:
        for f, m in keep.items():
            api("POST", "/api/v1/memories", {
                "content": f, "realm_hint": "cron-findings", "memory_type": "event",
                "tags": ["cron-finding", "backfilled"], "source": "hermes-cron",
            })
            stored += 1
    deleted = failed = 0
    for i, m in enumerate(cron, 1):
        try:
            api("DELETE", f"/api/v1/memories/{m['id']}")
            deleted += 1
        except subprocess.CalledProcessError:
            failed += 1
        if i % 1000 == 0:
            print(f"  ...{i}/{len(cron)}", flush=True)
    print(f"re-stored findings={stored} deleted={deleted} failed={failed}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
