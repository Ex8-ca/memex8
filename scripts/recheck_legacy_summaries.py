#!/usr/bin/env python3
"""Re-check legacy consolidation summaries against the originals that still exist.

Before provenance tracking, slumber consolidation wrote summaries without recording
what they were built from (`chunk_type == "consolidated"`, empty `consolidated_from`).
This script asks, for each such summary: is every fact in it still stated by an
original memory? If yes it is redundant and crowds the originals out of search; if
not, it may be the only copy of something and must be reviewed by a person.

  covered  every fact is quoted from an original (and the quote really appears there)
  partial  some facts are MISSING from the originals  -> review, never removed
  orphan   no similar originals found                  -> review, never removed

DRY RUN BY DEFAULT: it only writes a report. Removal needs --apply and
--yes-remove-covered, takes a JSON backup of each removed summary first, and touches
only `covered`, non-certified summaries. Restore one with
`memex8 import --path <backup.json>` (summaries are re-embedded).

PRIVACY: summary and original text is sent to the LLM at LLM_API_URL (the same
exposure consolidation itself had). Point it at a local model to keep it on your
network. The report and backup contain memory text: both are written mode 600;
keep them out of version control.

Env: MEMEX8_URL (default http://localhost:8080), MEMEX8_API_KEY,
     LLM_API_URL (OpenAI-compatible, no /v1 suffix), LLM_MODEL, LLM_API_KEY,
     LLM_THINKING=1 to let a reasoning model think (much slower, same task).
"""
import argparse
import concurrent.futures as cf
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

URL = os.environ.get("MEMEX8_URL", "http://localhost:8080").rstrip("/")
KEY = os.environ.get("MEMEX8_API_KEY", "")
LLM_URL = os.environ.get("LLM_API_URL", "").rstrip("/")
LLM_MODEL = os.environ.get("LLM_MODEL", "")
LLM_KEY = os.environ.get("LLM_API_KEY", os.environ.get("OPENAI_API_KEY", ""))

MIN_QUOTE_CHARS = 12
PRIORITY = {"profile": 0, "preference": 1}

PROMPT = """You are auditing a memory store. SUMMARY was written by an AI from older memories.
ORIGINALS are memories that still exist.

List each distinct fact stated in SUMMARY. For each fact, give the exact words from one ORIGINAL that state it
(a verbatim quote, at least a short phrase), or the single word MISSING if no ORIGINAL states it.
A fact is MISSING if an original only hints at it or states something different.

Answer with JSON only: {{"facts": [{{"fact": "...", "quote": "..."}}]}}

SUMMARY:
{summary}

ORIGINALS:
{originals}
"""


# ----- pure functions (unit-tested) -----

def parse_answer(text):
    """Pull the facts list out of an LLM reply; tolerate fences and chatter. None if unusable."""
    if not text:
        return None
    m = re.search(r"\{.*\}", text, re.S)
    if not m:
        return None
    try:
        data = json.loads(m.group(0))
    except json.JSONDecodeError:
        return None
    facts = data.get("facts") if isinstance(data, dict) else None
    if not isinstance(facts, list):
        return None
    out = []
    for f in facts:
        if isinstance(f, dict) and isinstance(f.get("fact"), str):
            out.append({"fact": f["fact"], "quote": str(f.get("quote", "MISSING"))})
    return out


def _norm(s):
    return re.sub(r"\s+", " ", s.lower()).strip()


def quote_supported(quote, originals):
    """True only if `quote` really appears (whitespace/case-insensitive) in some original.
    This stops a model from 'finding' support that is not there."""
    q = _norm(quote or "")
    if len(q) < MIN_QUOTE_CHARS or q == "missing":
        return False
    return any(q in _norm(t) for t in originals.values())


def classify(facts, originals):
    if not originals:
        return {"class": "orphan", "missing": []}
    if not facts:
        return {"class": "partial", "missing": ["(model returned no facts)"]}
    missing = [f["fact"] for f in facts if not quote_supported(f["quote"], originals)]
    return {"class": "partial" if missing else "covered", "missing": missing}


def is_legacy_summary(m):
    return m.get("chunk_type") == "consolidated" and not m.get("consolidated_from")


def is_certified(m):
    return bool(m.get("certified_at"))


def order_for_review(mems):
    return sorted(mems, key=lambda m: PRIORITY.get(m.get("memory_type") or "", 9))


def usable_originals(results, summary_ids, self_id):
    """Search results (id + content only: no chunk_type) -> {id: content} of true originals.
    Filter by the store's own set of consolidated ids; never trust a field the API omits."""
    return {m["id"]: m.get("content", "") for m in results
            if m["id"] != self_id and m["id"] not in summary_ids}


def originals_still_valid(ids, live):
    """Apply-time guard: every original must still exist and must not be a summary."""
    return bool(ids) and all(i in live and live[i].get("chunk_type") != "consolidated" for i in ids)


def write_backup(path, records):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(records, f, indent=1)
    os.chmod(path, 0o600)


# ----- network -----

def api(method, path, body=None):
    req = urllib.request.Request(
        URL + path, method=method, data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        raw = r.read()
        return json.loads(raw) if raw else {}


def fetch_all():
    out, offset = [], 0
    while True:
        page = api("GET", f"/api/v1/memories?limit=500&offset={offset}")
        out += page["memories"]
        offset += 500
        if offset >= page["total"] or not page["memories"]:
            return out


SUMMARY_IDS = set()  # every consolidated memory id, filled from the full store listing


def find_originals(summary):
    res = api("POST", "/api/v1/memories/search",
              {"query": summary["content"][:2000], "limit": 24, "min_score": 0.6})
    got = usable_originals(res.get("results", []), SUMMARY_IDS, summary["id"])
    return dict(list(got.items())[:12])


def ask_llm(summary, originals):
    body = {
        "model": LLM_MODEL, "temperature": 0,
        # Reasoning models can spend ~13k hidden tokens (2.5 min) per summary; this task is
        # quote matching, so thinking is off unless LLM_THINKING=1. Ignored by servers without it.
        **({} if os.environ.get("LLM_THINKING") == "1" else {"chat_template_kwargs": {"enable_thinking": False}}),
        "messages": [{"role": "user", "content": PROMPT.format(
            summary=summary["content"][:4000],
            originals="\n\n".join(f"[{i}] {t[:1500]}" for i, t in originals.items()))}],
    }
    headers = {"Content-Type": "application/json"}
    if LLM_KEY:
        headers["Authorization"] = f"Bearer {LLM_KEY}"
    for attempt in range(3):
        try:
            req = urllib.request.Request(f"{LLM_URL}/v1/chat/completions", data=json.dumps(body).encode(),
                                         headers=headers, method="POST")
            with urllib.request.urlopen(req, timeout=300) as r:
                return json.load(r)["choices"][0]["message"]["content"]
        except (urllib.error.URLError, TimeoutError, KeyError):
            time.sleep(3 * (attempt + 1))
    return None


def check_one(summary):
    originals = find_originals(summary)
    rec = {"id": summary["id"], "memory_type": summary.get("memory_type"),
           "certified": is_certified(summary), "originals": sorted(originals)}
    if not originals:
        rec.update(classify([], {}))
        return rec
    facts = parse_answer(ask_llm(summary, originals) or "")
    if facts is None:
        rec.update({"class": "error", "missing": []})
        return rec
    rec.update(classify(facts, originals))
    return rec


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", required=True, help="report file (mode 600)")
    p.add_argument("--types", help="comma list of memory_type to check (default all)")
    p.add_argument("--limit", type=int, help="check at most N summaries")
    p.add_argument("--workers", type=int, default=3)
    p.add_argument("--apply", action="store_true", help="remove covered summaries (needs --yes-remove-covered)")
    p.add_argument("--yes-remove-covered", action="store_true")
    p.add_argument("--backup", help="JSON backup of removed summaries (required with --apply)")
    p.add_argument("--from-report", help="with --apply: act on this earlier dry-run report instead of re-checking")
    a = p.parse_args()

    if not KEY:
        sys.exit("MEMEX8_API_KEY is not set")
    if a.apply and not (a.yes_remove_covered and a.backup):
        sys.exit("--apply needs --yes-remove-covered and --backup <file>")

    if a.from_report:
        records = json.load(open(a.from_report))["records"]
    else:
        if not (LLM_URL and LLM_MODEL):
            sys.exit("LLM_API_URL and LLM_MODEL must be set")
        mems = fetch_all()
        SUMMARY_IDS.update(m["id"] for m in mems if m.get("chunk_type") == "consolidated")
        todo = [m for m in mems if is_legacy_summary(m)]
        if a.types:
            keep = set(a.types.split(","))
            todo = [m for m in todo if (m.get("memory_type") or "") in keep]
        todo = order_for_review(todo)[: a.limit]
        print(f"{len(todo)} legacy summaries to check (of {len(mems)} memories)", flush=True)
        records = []
        with cf.ThreadPoolExecutor(a.workers) as ex:
            for i, rec in enumerate(ex.map(check_one, todo), 1):
                records.append(rec)
                if i % 10 == 0 or i == len(todo):
                    print(f"  {i}/{len(todo)}", flush=True)
        fd = os.open(a.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump({"records": records}, f, indent=1)
        os.chmod(a.out, 0o600)

    counts = {}
    for r in records:
        counts[r["class"]] = counts.get(r["class"], 0) + 1
    print("classes:", counts, "| report:", a.out)

    if not a.apply:
        print("dry run: nothing changed")
        return

    covered = [r for r in records if r["class"] == "covered" and not r.get("certified")]
    live = {m["id"]: m for m in fetch_all()}
    unsafe = [r for r in covered if not originals_still_valid(r["originals"], live)]
    if unsafe:
        sys.exit(f"REFUSING to apply: {len(unsafe)} covered summaries cite originals that no longer exist "
                 f"or are themselves summaries. Re-run the check; nothing was changed.")
    backup = [api("GET", f"/api/v1/memories/{r['id']}") for r in covered]
    write_backup(a.backup, backup)
    print(f"backed up {len(backup)} summaries to {a.backup}")
    for r in covered:
        api("DELETE", f"/api/v1/memories/{r['id']}")
    print(f"removed {len(covered)} covered summaries")


if __name__ == "__main__":
    main()
