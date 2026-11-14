#!/usr/bin/env python3
"""Download the DeriveMem public data used by the memex8 retrieval bench.

Files are fetched one by one through the Anonymous GitHub file API (directory
listings of subfolders are not served). Data is CC BY 4.0. Output goes to
bench/memderive/data/<world>/ and is git-ignored.

Usage: fetch.py [world_id ...]   (default: the ten headline worlds)
"""
import json
import pathlib
import sys
import time
import urllib.error
import urllib.request

REPO = "DeriveMem-67C6"
API = f"https://anonymous.4open.science/api/repo/{REPO}/file/"
FILES = ["chronicle.jsonl", "queries.jsonl", "gold.json", "meta.json"]
OUT = pathlib.Path(__file__).resolve().parent / "data"


UA = "memex8-bench/0.1 (+https://github.com/Ex8-ca/memex8)"


def get(path: str, attempts: int = 4) -> bytes:
    """GET one file. The host rejects urllib's default User-Agent with 403, and
    rate-limits bursts, so send a real UA and back off on 403/429/5xx."""
    req = urllib.request.Request(API + path, headers={"User-Agent": UA})
    for i in range(attempts):
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code not in (403, 429, 500, 502, 503) or i == attempts - 1:
                raise RuntimeError(f"{path}: HTTP {e.code}") from e
            time.sleep(2 * (i + 1))
    raise RuntimeError(f"{path}: unreachable")


def headline_worlds(worlds_json: dict) -> list[str]:
    """The paper's headline suite: dev worlds plus held-out worlds (10 total).

    The dialogue rendering `tripops_w1d` is a surface variant excluded from the
    headline aggregate, and is not listed under `protocol`.
    """
    protocol = worlds_json["protocol"]
    return list(protocol["dev_worlds"]) + list(protocol["held_out"])


def check(world_dir: pathlib.Path) -> tuple[int, int]:
    """Parse every file once; return (events, queries). Raises on bad data."""
    events = [json.loads(l) for l in (world_dir / "chronicle.jsonl").read_text().splitlines() if l.strip()]
    queries = [json.loads(l) for l in (world_dir / "queries.jsonl").read_text().splitlines() if l.strip()]
    gold = json.loads((world_dir / "gold.json").read_text())
    if len(gold) != len(queries):
        raise ValueError(f"{world_dir.name}: {len(gold)} gold items vs {len(queries)} queries")
    return len(events), len(queries)


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    worlds_raw = get("benchmark/worlds.json")
    (OUT / "worlds.json").write_bytes(worlds_raw)
    ids = sys.argv[1:] or headline_worlds(json.loads(worlds_raw))
    total_e = total_q = 0
    for w in ids:
        d = OUT / w
        d.mkdir(exist_ok=True)
        for f in FILES:
            body = get(f"benchmark/data/{w}/{f}")
            if f.endswith(".json"):
                json.loads(body)  # fail fast on an HTML error page
            (d / f).write_bytes(body)
        e, q = check(d)
        total_e += e
        total_q += q
        print(f"{w}: {e} events, {q} queries")
    print(f"total: {len(ids)} worlds, {total_e} events, {total_q} queries")
    return 0


if __name__ == "__main__":
    sys.exit(main())
