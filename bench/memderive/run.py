#!/usr/bin/env python3
"""MemDerive retrieval bench for memex8 (no LLM judge, no answer model).

For each world: start a fresh isolated memex8 (instance.sh), ingest every event
as one memory, run every query, and score whether the top-k retrieved events
contain the gold support and witnesses. The same events are also ranked by an
event-level BM25 reference (bm25_ref.py), the paper's strongest baseline.

Usage:
  MEMEX8_BENCH_ENV=/path/.env.memex8 run.py --image memex8:<sha> --label baseline [--worlds w ...]
        [--extra-toml file] [--keep]
Writes results/<UTC timestamp>-<label>.json and prints a markdown table.
"""
import argparse
import concurrent.futures as cf
import datetime as dt
import json
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

from bm25_ref import BM25
from metrics import score, summarize

HERE = pathlib.Path(__file__).resolve().parent
DATA = HERE / "data"
RESULTS = HERE / "results"
KS = (15, 50)
WORKERS = 8


def load_world(world: str):
    d = DATA / world
    events = [json.loads(l) for l in (d / "chronicle.jsonl").read_text().splitlines() if l.strip()]
    queries = [json.loads(l) for l in (d / "queries.jsonl").read_text().splitlines() if l.strip()]
    gold = {g["query_id"]: g for g in json.loads((d / "gold.json").read_text())}
    return events, queries, gold


def event_text(e: dict) -> str:
    # The time prefix gives the same temporal cue a real ingested memory carries.
    return f"[{e['time']}] {e['text']}"


class Api:
    def __init__(self, base: str, key: str):
        self.base, self.key = base.rstrip("/"), key

    def post(self, path: str, body: dict, attempts: int = 5) -> dict:
        data = json.dumps(body).encode()
        for i in range(attempts):
            req = urllib.request.Request(
                self.base + path,
                data=data,
                method="POST",
                headers={"Authorization": f"Bearer {self.key}", "Content-Type": "application/json"},
            )
            try:
                with urllib.request.urlopen(req, timeout=120) as r:
                    return json.load(r)
            except urllib.error.HTTPError as e:
                if e.code in (429, 500, 502, 503, 504) and i < attempts - 1:
                    time.sleep(1.5 * (i + 1))
                    continue
                raise RuntimeError(f"POST {path}: HTTP {e.code} {e.read()[:200]!r}") from e
            except (urllib.error.URLError, TimeoutError):
                if i < attempts - 1:
                    time.sleep(1.5 * (i + 1))
                    continue
                raise
        raise RuntimeError("unreachable")


def instance(*args: str) -> str:
    out = subprocess.run([str(HERE / "instance.sh"), *args], check=True, capture_output=True, text=True)
    return out.stdout.strip()


def run_world(world: str, image: str, extra: str | None, port: int) -> dict:
    events, queries, gold = load_world(world)
    instance("down")
    instance("up", image, *([extra] if extra else []))
    api = Api(f"http://127.0.0.1:{port}", instance("key"))

    # Ingest: one memory per event; keep memory id -> event id.
    id_to_event: dict[str, str] = {}
    t0 = time.time()

    def ingest(e):
        r = api.post("/api/v1/memories", {"content": event_text(e), "tags": ["bench"], "memory_type": "event"})
        return r["id"], e["event_id"]

    with cf.ThreadPoolExecutor(WORKERS) as ex:
        for mid, eid in ex.map(ingest, events):
            id_to_event[mid] = eid
    ingest_s = time.time() - t0

    # Retrieve with memex8.
    t0 = time.time()

    def search(q):
        r = api.post("/api/v1/memories/search", {"query": q["question"], "limit": max(KS), "min_score": 0.0})
        return q["query_id"], [m["id"] for m in r.get("results", [])]

    with cf.ThreadPoolExecutor(WORKERS) as ex:
        retrieved = dict(ex.map(search, queries))
    search_s = time.time() - t0

    # Reference BM25 over the same texts.
    bm25 = BM25({e["event_id"]: event_text(e) for e in events})

    rows = {f"{name}@{k}": [] for name in ("memex8", "bm25") for k in KS}
    unmapped = 0
    for q in queries:
        g = gold[q["query_id"]]
        mem_ids = retrieved[q["query_id"]]
        unmapped += sum(1 for m in mem_ids if m not in id_to_event)
        mem_events = [id_to_event[m] for m in mem_ids if m in id_to_event]
        ref_events = bm25.search(q["question"], max(KS))
        for k in KS:
            rows[f"memex8@{k}"].append(score(mem_events[:k], g))
            rows[f"bm25@{k}"].append(score(ref_events[:k], g))

    return {
        "world": world,
        "events": len(events),
        "queries": len(queries),
        "ingested": len(id_to_event),
        "unmapped_results": unmapped,
        "ingest_seconds": round(ingest_s, 1),
        "search_seconds": round(search_s, 1),
        "rows": rows,
    }


def table(summary: dict) -> str:
    lines = ["| system | n | support recall | witness recall | full witness | obsolete hit |", "|---|---:|---:|---:|---:|---:|"]
    for name, s in summary.items():
        a = s["all"]
        lines.append(
            f"| {name} | {a['n']} | {a['support_recall']} | {a['witness_recall']} | {a['full_witness']} | {a['obsolete_hit']} |"
        )
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--image", required=True)
    p.add_argument("--label", required=True)
    p.add_argument("--worlds", nargs="*")
    p.add_argument("--extra-toml")
    p.add_argument("--port", type=int, default=18080)
    p.add_argument("--keep", action="store_true", help="leave the last instance running")
    a = p.parse_args()

    worlds = a.worlds or json.loads((DATA / "worlds.json").read_text())["protocol"]["dev_worlds"] + json.loads(
        (DATA / "worlds.json").read_text()
    )["protocol"]["held_out"]
    per_world, all_rows = [], {}
    try:
        for w in worlds:
            r = run_world(w, a.image, a.extra_toml, a.port)
            per_world.append({k: v for k, v in r.items() if k != "rows"} | {"summary": {n: summarize(rs) for n, rs in r["rows"].items()}})
            for n, rs in r["rows"].items():
                all_rows.setdefault(n, []).extend(rs)
            m15 = per_world[-1]["summary"]["memex8@15"]["all"]["support_recall"]
            b15 = per_world[-1]["summary"]["bm25@15"]["all"]["support_recall"]
            print(f"{w}: memex8@15 support {m15}  bm25@15 support {b15}  "
                  f"(ingest {r['ingest_seconds']}s, search {r['search_seconds']}s, unmapped {r['unmapped_results']})",
                  flush=True)
    finally:
        if not a.keep:
            instance("down")

    overall = {n: summarize(rs) for n, rs in all_rows.items()}
    RESULTS.mkdir(exist_ok=True)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M")
    out = RESULTS / f"{stamp}-{a.label}.json"
    out.write_text(json.dumps({
        "label": a.label,
        "image": a.image,
        "extra_toml": pathlib.Path(a.extra_toml).read_text() if a.extra_toml else None,
        "worlds": worlds,
        "ks": KS,
        "notes": [
            "Fresh instance per world with slumber off; no TurboVec index, so dense search is the exact Qdrant path.",
            "Production uses a 4-bit TurboVec index and may score slightly lower.",
            "Searches 'touch' memories (access count, importance); query order is fixed per world.",
        ],
        "overall": overall,
        "per_world": per_world,
    }, indent=2))
    print(f"\n{table(overall)}\n\nresults: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
