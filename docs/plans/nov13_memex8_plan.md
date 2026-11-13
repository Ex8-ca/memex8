# memex8 retrieval-quality plan (Nov 13 2026)

> **For Hermes:** implement phase by phase. One PR per phase. Use TDD inside each task.
> Deploy each phase with the backup-gated upgrade (`memex8-update` skill, "Live deploy path"),
> then verify against the symptom the phase targets, not just container health.

**Goal:** measure how well memex8 *gathers* the memories a question needs, then raise that
number with keyword (BM25) search alongside vector search. Stop consolidation from blending
durable facts and rules into summaries, re-check the legacy summaries, and retire consolidation
if it isn't earning its place. Add an optional "dream" job that weaves past memories into a short
narrative, kept apart from factual memory.

**Architecture:** a stdlib-Python benchmark harness runs an isolated memex8 instance against the
public MemDerive/DeriveMem data and scores retrieval with no LLM. On the server, a small
in-process BM25 index (rebuilt from Qdrant at startup and after slumber, updated on writes)
adds lexical candidates that are re-scored with real cosine similarity and blended in. A
one-line filter keeps long-lived memory types out of consolidation, and a config switch can turn
consolidation off. Dreams live in their own store and API, never in the `memories` collection,
so they can never come back from search as facts.

**Tech stack:** Rust (axum, qdrant-client 1.17 locked), Qdrant 1.17, Python 3 stdlib for the bench.

**Source paper:** *MemDerive: A Diagnostic Benchmark for Agent Memory over Evolving Data*,
arXiv:2610.25373 (Oct 2026). Data: DeriveMem (CC BY 4.0), `https://anonymous.4open.science/r/DeriveMem-67C6`.

---

## 0. What we verified before writing this (Nov 13)

| Fact | Evidence |
|---|---|
| Search is vector-only | `src/engine/mod.rs::search_embedded` uses TurboVec or `store.search_filtered`; zero BM25/sparse/full-text code in `src/`. The code itself says "memex8 has no FTS layer" (query-intent comment). |
| Consolidation input has one choke point | `src/engine/dedup.rs::consolidation_inputs` (excludes summaries, certified, cooldown). No memory-type exclusion. |
| Consolidation has already absorbed durable facts | In a long-running production store, about 7.5% of memories are summaries (`chunk_type=consolidated`), and nearly all `profile` memories and most `preference` memories are summaries rather than originals. |
| Legacy summaries can't be traced | Every existing summary predates provenance tracking: none has `consolidated_from` or `derived_by`, so they can't be undone through the unconsolidate API. |
| Consolidation has been silent for weeks | No summary has been created since 2026-10-30 in that store. |
| Likely cause of the silence | `scheduler.rs::should_run_at_schedule` matches `consolidation_schedule` (`0 3 * * *`) with a ±5-minute window, which assumes a slumber tick every 5 minutes. With `cron_ingest = "0 * * * *"`, ticks are hourly and offset from container start (`tokio::time::interval`), so a tick rarely lands between 02:55 and 03:05. To be confirmed in Task 1.4. |
| Rule-like types are rarely assigned | Most memories are typed `general` or `event`; `instruction`, `decision` and `commitment` barely occur, so type-based protection mostly covers `profile`, `preference`, `relationship` and `setup`. |
| Slumber in the daemon goes through one method | `Engine::trigger_slumber` (`src/engine/mod.rs`), called by `src/engine/scheduler.rs`. `memex8 slumber trigger` (CLI) is a separate process. |
| Bench data is downloadable | `https://anonymous.4open.science/api/repo/DeriveMem-67C6/file/<path>` returns files (HTTP 200); the `/zip` endpoint returns `application/zip`. Directory listing of subfolders fails (`not_connected`), so fetch by known paths. |
| Bench data has retrieval gold | `benchmark/data/<world>/gold.json` items carry `witness`, `alternative_witnesses`, `valid_support`, `invalid_evidence` (obsolete), `distractors`, `family`, `composition_degree`. Events: `chronicle.jsonl` (`event_id`, `time`, `source`, `text`, `session_id`). Queries: `queries.jsonl` (`query_id`, `as_of`, `question`). |

## Decisions

1. **Measure retrieval, not answers.** MemDerive's main finding is that systems fail at *gathering*.
   We score whether memex8's top-k contains the gold support and witnesses. No answer model and no
   LLM judge, so the scores are deterministic and nearly free. (Answer accuracy is out of scope.)
2. **BM25 in process, not Qdrant sparse vectors.** Qdrant can't add a new vector to an existing
   collection, so sparse vectors would mean migrating the live `memories` collection. The corpus is
   ~6k short memories, so an in-memory inverted index is small and fast, and it can be rebuilt from Qdrant
   at any time. It's a cache, so failures degrade to today's dense-only search.
3. **Blend scores on the cosine scale, not reciprocal-rank fusion.** RRF scores (~0.016) would
   break `min_score`, the coverage verdict and the intent re-weighting, which all assume cosine-like
   scores. Every candidate keeps its true cosine; lexical matches add a bonus
   `lexical_weight × bm25 / max_bm25`.
4. **Hybrid ships off by default.** It turns on only after the bench shows a gain (Phase 2 gate).
5. **Protect by memory type, not by content guessing.** This is cheap and predictable. Better typing is
   a separate follow-up (`scripts/classify_memory_types.py`).
6. **Hybrid search becomes the default for everyone** once it passes the Phase 2 gate. It ships off first
   only so the gate can be measured.
7. **Retire consolidation unless it proves useful** (Task 1.5 criteria). Retiring means a config switch
   (`slumber.consolidation.enabled`, default `false`); the code and the undo API stay.
8. **Re-check legacy summaries; never delete them blindly.** Summaries fully covered by surviving originals
   get archived (reversible). The rest are flagged for review (Phase 1b).
9. **Dreams are opt-in and quarantined.** They're off by default, run on their own wall-clock schedule (not the slumber
   tick that broke consolidation), get stored outside `memories`, and are never returned by search, recall or MCP.

## Non-goals

- Datalog, derivation DAGs or a rule engine inside memex8.
- Migrating Qdrant collections.
- Rewriting legacy summaries. (Phase 1b removes only summaries whose every fact is in an original, after exporting them.)
- LLM answer accuracy on MemDerive.

---

## Phase 0: Benchmark harness and baseline (PR 1)

New folder `bench/memderive/` (stdlib Python only). Downloaded data and results stay out of git.

### Task 0.1: Ignore bench data and results

**Files:** Modify `.gitignore`

```gitignore
# MemDerive bench: downloaded data and run output stay local
bench/memderive/data/
bench/memderive/results/
bench/memderive/instance/
```

Commit: `git add .gitignore && git commit -m "bench: ignore MemDerive data and results"`

### Task 0.2: Data fetcher

**Files:** Create `bench/memderive/fetch.py`

The world IDs come from `benchmark/worlds.json`. Read its structure first and adjust `world_ids()` to the real
key. (We confirmed `tripops_w1`, `shopops_w1`, `eduops_w1` exist; the other seven are held out.)

```python
#!/usr/bin/env python3
"""Download the DeriveMem public data used by the memex8 retrieval bench.

Files are fetched one by one through the Anonymous GitHub file API (directory
listings of subfolders are not served). Data is CC BY 4.0. Output goes to
bench/memderive/data/<world>/ and is git-ignored.
"""
import json
import pathlib
import sys
import urllib.request

REPO = "DeriveMem-67C6"
API = f"https://anonymous.4open.science/api/repo/{REPO}/file/"
FILES = ["chronicle.jsonl", "queries.jsonl", "gold.json", "meta.json"]
OUT = pathlib.Path(__file__).resolve().parent / "data"


def get(path: str) -> bytes:
    with urllib.request.urlopen(API + path, timeout=60) as r:
        if r.status != 200:
            raise RuntimeError(f"{path}: HTTP {r.status}")
        return r.read()


def world_ids(worlds_json: dict) -> list[str]:
    # Adjust to the real schema of benchmark/worlds.json after inspecting it.
    worlds = worlds_json.get("worlds") or worlds_json
    if isinstance(worlds, dict):
        return sorted(worlds.keys())
    return sorted(w["id"] if isinstance(w, dict) else w for w in worlds)


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    meta = get("benchmark/worlds.json")
    (OUT / "worlds.json").write_bytes(meta)
    ids = sys.argv[1:] or [w for w in world_ids(json.loads(meta)) if not w.endswith("d")]
    for w in ids:
        d = OUT / w
        d.mkdir(exist_ok=True)
        for f in FILES:
            body = get(f"benchmark/data/{w}/{f}")
            if f.endswith(".json"):
                json.loads(body)  # fail fast on HTML error pages
            (d / f).write_bytes(body)
        print(f"{w}: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

The `w.endswith("d")` filter drops `tripops_w1d`, the dialogue variant the paper excludes. Verify it doesn't
also drop a real world ID.

Run: `python3 -I bench/memderive/fetch.py tripops_w1` → expected `tripops_w1: ok`, plus 452 lines in
`data/tripops_w1/chronicle.jsonl` and 140 in `queries.jsonl`.

### Task 0.3: Metrics module (TDD)

**Files:** Create `bench/memderive/metrics.py` and `bench/memderive/test_metrics.py`

```python
"""Retrieval metrics against DeriveMem gold. Pure functions, no I/O."""


def support_recall(retrieved: list[str], gold: dict) -> float:
    support = set(gold.get("valid_support") or gold.get("gold_evidence") or [])
    return len(support & set(retrieved)) / len(support) if support else 1.0


def witnesses(gold: dict) -> list[list[str]]:
    ws = [gold.get("witness") or []] + list(gold.get("alternative_witnesses") or [])
    return [w for w in ws if w]


def witness_recall(retrieved: list[str], gold: dict) -> float:
    got = set(retrieved)
    ws = witnesses(gold)
    return max(len(got & set(w)) / len(w) for w in ws) if ws else 1.0


def full_witness(retrieved: list[str], gold: dict) -> bool:
    got = set(retrieved)
    ws = witnesses(gold)
    return any(set(w) <= got for w in ws) if ws else True


def obsolete_hit(retrieved: list[str], gold: dict) -> bool:
    return bool(set(gold.get("invalid_evidence") or []) & set(retrieved))


def summarize(rows: list[dict]) -> dict:
    """rows: [{"family":..., "support":float, "witness":float, "full":bool, "obsolete":bool}]"""
    def agg(rs):
        n = len(rs) or 1
        return {
            "n": len(rs),
            "support_recall": round(100 * sum(r["support"] for r in rs) / n, 2),
            "witness_recall": round(100 * sum(r["witness"] for r in rs) / n, 2),
            "full_witness": round(100 * sum(r["full"] for r in rs) / n, 2),
            "obsolete_hit": round(100 * sum(r["obsolete"] for r in rs) / n, 2),
        }
    out = {"all": agg(rows)}
    for fam in sorted({r["family"] for r in rows}):
        out[fam] = agg([r for r in rows if r["family"] == fam])
    return out
```

```python
import unittest
from metrics import support_recall, witness_recall, full_witness, obsolete_hit, summarize

G = {"valid_support": ["e1", "e2", "e3", "e4"], "witness": ["e3", "e4"],
     "alternative_witnesses": [["e1", "e9"]], "invalid_evidence": ["e7"]}

class T(unittest.TestCase):
    def test_support_recall(self):
        self.assertEqual(support_recall(["e1", "e3", "x"], G), 0.5)
    def test_witness_recall_takes_best_witness(self):
        self.assertEqual(witness_recall(["e3", "e1"], G), 0.5)
        self.assertEqual(witness_recall(["e3", "e4"], G), 1.0)
    def test_full_witness_accepts_alternative(self):
        self.assertTrue(full_witness(["e1", "e9", "zz"], G))
        self.assertFalse(full_witness(["e1", "e3"], G))
    def test_obsolete_hit(self):
        self.assertTrue(obsolete_hit(["e7"], G))
        self.assertFalse(obsolete_hit(["e1"], G))
    def test_empty_gold_is_vacuous(self):
        self.assertEqual(support_recall([], {}), 1.0)
        self.assertTrue(full_witness([], {}))
    def test_summarize_by_family(self):
        rows = [{"family": "temporal", "support": 1.0, "witness": 1.0, "full": True, "obsolete": False},
                {"family": "rules", "support": 0.0, "witness": 0.5, "full": False, "obsolete": True}]
        s = summarize(rows)
        self.assertEqual(s["all"]["support_recall"], 50.0)
        self.assertEqual(s["rules"]["obsolete_hit"], 100.0)

if __name__ == "__main__":
    unittest.main()
```

Run first with an empty `metrics.py` → ImportError. Then implement it and run
`cd bench/memderive && python3 -I -m unittest -q test_metrics` → `OK` (6 tests).

### Task 0.4: Reference BM25 baseline in Python

**Files:** Create `bench/memderive/bm25_ref.py` and add tests to `test_metrics.py`.

This reproduces the paper's BM25 condition at event level, as a sanity anchor. Our metric
should show BM25 ≥ dense on derived-state support, as the paper found. If it doesn't, check the harness
before believing any memex8 number.

```python
"""Event-level BM25 over a chronicle. Same tokenizer rules as src/engine/lexical.rs."""
import math
import re
from collections import Counter

STOP = set("a an and are as at be by for from has have in is it its of on or that the this to was were will with".split())


def tokenize(text: str) -> list[str]:
    toks = re.split(r"[^0-9a-z]+", text.lower())
    return [t for t in toks if t and t not in STOP and (len(t) > 1 or t.isdigit())]


class BM25:
    def __init__(self, docs: dict[str, str], k1: float = 1.2, b: float = 0.75):
        self.k1, self.b = k1, b
        self.tf = {i: Counter(tokenize(t)) for i, t in docs.items()}
        self.len = {i: sum(c.values()) for i, c in self.tf.items()}
        self.avg = (sum(self.len.values()) / len(self.len)) if self.len else 0.0
        self.df = Counter(t for c in self.tf.values() for t in c)
        self.n = len(self.tf)

    def search(self, query: str, k: int) -> list[str]:
        q = set(tokenize(query))
        scores = {}
        for i, c in self.tf.items():
            s = 0.0
            for t in q:
                f = c.get(t)
                if not f:
                    continue
                idf = math.log(1 + (self.n - self.df[t] + 0.5) / (self.df[t] + 0.5))
                s += idf * f * (self.k1 + 1) / (f + self.k1 * (1 - self.b + self.b * self.len[i] / self.avg))
            if s > 0:
                scores[i] = s
        return [i for i, _ in sorted(scores.items(), key=lambda x: (-x[1], x[0]))[:k]]
```

Tests to add: tokenize drops stopwords and keeps digits (`tokenize("The Trip-Grus to Corby 2")` →
`["trip", "grus", "corby", "2"]`); a doc containing the rare query term ranks above one with only common
terms; a query with no matches returns `[]`.

### Task 0.5: Isolated instance script

**Files:** Create `bench/memderive/instance.sh`

This is the pattern already proven during the Nov 13 install test: a clean clone, its own compose project, ports 18080/18081,
its own HOME and volumes. Per-command `env HOME=...`; never `export HOME` in a persistent shell.

```bash
#!/usr/bin/env bash
# Usage: instance.sh up <image> | down
# Starts an isolated memex8 + Qdrant for benchmarking. Never touches the live stack.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"; repo="$(cd "$here/../.." && pwd)"
inst="$here/instance"; proj=memex8bench
envsrc="${MEMEX8_BENCH_ENV:?set MEMEX8_BENCH_ENV to an .env.memex8 holding OPENAI_API_KEY}"
compose() {
  env HOME="$inst/home" docker compose -p "$proj" \
    -f "$repo/docker-compose.yml" -f "$inst/override.yml" "$@"
}
case "${1:-}" in
  up)
    image="${2:?image tag, e.g. memex8:<sha>}"
    mkdir -p "$inst/home/.memex8"
    umask 077
    grep -v '^MEMEX8_' "$envsrc" > "$inst/.env.memex8"
    echo "MEMEX8_API_KEY=$(python3 -c 'import secrets;print(secrets.token_urlsafe(32))')" >> "$inst/.env.memex8"
    # Slumber off: no consolidation, dedup or archive during a run.
    sed -e 's/^idle_timeout = .*/idle_timeout = "9999h"/' \
        -e 's/^cron_ingest = .*/cron_ingest = "0 0 1 1 *"/' \
        "$repo/config.example.toml" > "$inst/config.toml"
    cat > "$inst/override.yml" <<EOF
services:
  memex8:
    image: $image
    env_file: !override ["$inst/.env.memex8"]
    volumes: !override
      - $inst/config.toml:/config.toml:ro
      - memex8_data:/var/lib/memex8
      - $inst/home/.memex8:/home/memex8/.memex8
    ports: !override
      - "127.0.0.1:18080:8080"
      - "127.0.0.1:18081:8081"
EOF
    compose up -d --no-build
    for _ in $(seq 1 60); do
      [ "$(docker inspect -f '{{.State.Health.Status}}' ${proj}-memex8-1 2>/dev/null)" = healthy ] && exit 0
      sleep 3
    done
    echo "bench instance did not become healthy" >&2; exit 1 ;;
  down)
    compose down -v
    docker run --rm -v "$inst:/w" alpine sh -c 'rm -rf /w/*' ;;
  *) echo "usage: $0 up <image> | down" >&2; exit 2 ;;
esac
```

Check against `config.example.toml`: the `sed` keys must match its real `[slumber]` lines. Grep
for `idle_timeout` and `cron_ingest` and adjust if they differ. `consolidation_schedule` is separate; set it to
a never-firing cron the same way.

Verify: `MEMEX8_BENCH_ENV=<path to a copy of .env.memex8> bench/memderive/instance.sh up memex8:<live sha>`
→ exits 0; `curl -s 127.0.0.1:18080/health` → `OK`; live `memex8-memex8-1` StartedAt is unchanged.

### Task 0.6: Runner

**Files:** Create `bench/memderive/run.py`

For each world: restart the instance (fresh volumes), ingest every event as one memory, keep
`memory_id → event_id`, run every query with `limit = max(k)` and `min_score = 0`, then score at
k = 15 (the paper's k) and k = 50. Also run `bm25_ref` on the same chronicle.

- Memory content: `"[<time>] <text>"` (time prefix gives temporal cues, like a real ingested memory).
- Store body: `{"content": ..., "tags": ["bench"], "memory_type": "event"}`. Read the API key from
  `bench/memderive/instance/.env.memex8`.
- Search: `POST /api/v1/memories/search {"query": q, "limit": 50, "min_score": 0}`, ids from `results[].id`.
- Map ids → events with the dict built at ingest. Unmapped ids (shouldn't happen) are counted and reported.
- Output: `results/<YYYYMMDD-HHMM>-<label>.json` with per-world and overall `summarize()` for
  `memex8@15`, `memex8@50`, `bm25@15`, `bm25@50`, plus the image tag, worlds and counts.
- Also print a markdown table.

Known limits (state these in the output JSON): a fresh instance has no TurboVec index, so dense
search uses the exact Qdrant path. Production uses 4-bit TurboVec and may score slightly lower.
Searches also "touch" memories, but slumber is off, so this has no effect on the run.

Run (dev worlds first): `python3 -I bench/memderive/run.py --image memex8:<sha> --label baseline
--worlds tripops_w1 shopops_w1 eduops_w1`.

### Task 0.7: Baseline and sanity gate

1. Run the dev worlds and then all ten. Record the result file paths in this plan.
2. **Sanity gate:** `bm25@15 support_recall` should be at or above `memex8@15` overall, matching the paper. If dense
   wins by a lot, stop and investigate (mapping bug, wrong ids, wrong k) before Phase 2.
3. Commit the harness (not data or results). PR title: `bench: MemDerive retrieval harness (no LLM)`.

---

## Phase 1: Keep durable facts and rules out of consolidation (PR 2)

### Task 1.1: Failing test

**Files:** `src/engine/dedup.rs` (tests module)

```rust
fn typed(id: &str, memory_type: &str) -> MemoryPoint {
    serde_json::from_value(serde_json::json!({
        "id": id, "content": format!("memory {id}"), "summary": null,
        "source_file": null, "realm_id": "r1", "realm_name": "Realm",
        "importance": 1.0, "upvotes": 0, "tags": [],
        "ingested_at": "2026-11-01T00:00:00Z", "last_accessed": "",
        "access_count": 0, "chunk_type": "manual", "heading": null,
        "source_hash": "h", "memory_type": memory_type, "visibility": "private"
    }))
    .unwrap()
}

#[test]
fn durable_types_are_never_consolidation_inputs() {
    let protected: Vec<MemoryPoint> = CONSOLIDATION_PROTECTED_TYPES
        .iter()
        .map(|t| typed(t, t))
        .collect();
    let refs: Vec<&MemoryPoint> = protected.iter().collect();
    assert!(consolidation_inputs(&refs).is_empty());

    let general = typed("g", "general");
    let event = typed("e", "event");
    let empty = typed("x", "");
    assert_eq!(consolidation_inputs(&[&general, &event, &empty]).len(), 3);
}
```

Run: `cargo test --release --lib durable_types_are_never_consolidation_inputs` → fails to compile
(`CONSOLIDATION_PROTECTED_TYPES` not found).

### Task 1.2: Implement

**Files:** `src/engine/dedup.rs` (near `consolidation_inputs`, ~line 188)

```rust
/// Memory types that hold durable facts and rules. Consolidation must never blend
/// them into a summary: a summary can drop an exception or an exact value, and
/// every later answer inherits the loss (MemDerive, arXiv:2610.25373, lesson L2).
pub const CONSOLIDATION_PROTECTED_TYPES: &[&str] = &[
    "profile",
    "preference",
    "relationship",
    "setup",
    "instruction",
    "decision",
    "commitment",
];

pub fn consolidation_protected_type(memory: &MemoryPoint) -> bool {
    CONSOLIDATION_PROTECTED_TYPES.contains(&memory.memory_type.as_str())
}
```

and add `&& !consolidation_protected_type(m)` to the filter in `consolidation_inputs`.

Run the new test plus the existing consolidation tests:
`cargo test --release --lib dedup` → all pass. Then `cargo fmt --all`.

### Task 1.3: Document

**Files:** `README.md` (Memory Type Reference section). Add one sentence: "Slumber never consolidates
`profile`, `preference`, `relationship`, `setup`, `instruction`, `decision` or `commitment` memories;
they keep their exact wording."

### Task 1.4: Confirm why consolidation went silent (read-only, time-boxed 30 min)

Hypothesis (Section 0): the ±5-minute window around 03:00 assumes 5-minute slumber ticks, but ticks are
hourly and offset from container start.

1. Unit-test the hypothesis without touching the scheduler: add a pure helper
   `fn tick_hits_window(start: NaiveTime, interval_min: u32, target: NaiveTime, tolerance_min: u32) -> bool`
   in a test module, and show that with `interval_min = 60` most start times never hit 03:00 ± 5 while
   `interval_min = 5` always does. (Test-only; it documents the bug.)
2. On the live daemon: `docker logs <container> | grep -E 'Scheduler started|phase 6|Consolidat'` shows
   the tick interval and whether phase 6 ever ran since the last restart.
3. `GET /api/v1/slumber/status` → `memories_consolidated` in the last report.
4. Write the result under "Findings".

### Task 1.5: Decide whether consolidation stays (measured, not guessed)

Consolidation stays only if **both** of these hold. Otherwise it is retired (Task 1.6).

- **It is used.** Over the store, summaries' mean `access_count` per memory is at least that of non-summaries,
  *and* summaries appear in the top-5 of at least 10% of a sample of 50 recent real queries (replay them with
  `POST /api/v1/memories/search`; use the queries' text only, don't store them).
- **It adds information.** In Phase 1b, at least 25% of summaries turn out *not* to be fully covered by surviving
  originals (that is, they hold the only copy of something). If almost all are redundant copies, consolidation
  only adds duplicates that crowd the top-k.

Record the numbers and the verdict under "Findings".

### Task 1.6: Retirement switch (do this if Task 1.5 says retire; it's cheap enough to add anyway)

**Files:** `src/config.rs` (`ConsolidationConfig`), `src/engine/slumber.rs` (phase 6, ~line 146),
`src/api/settings.rs` (`FIELDS`), `config.example.toml`, `README.md`.

1. Test first (`src/config.rs` tests): `AppConfig::default().slumber.consolidation.enabled == false`, and a
   TOML with `[slumber.consolidation] enabled = true` parses to `true`.
2. Add the field:

```rust
pub struct ConsolidationConfig {
    /// Run LLM consolidation (slumber phase 6). Off by default: summaries tend to
    /// drop exceptions and exact values, and they crowd originals out of the top-k.
    #[serde(default)]
    pub enabled: bool,
    // ...existing backend / model fields...
}
```

3. Gate phase 6: change `if self.config.slumber.consolidation_schedule.is_empty()` to
   `if !self.config.slumber.consolidation.enabled || self.config.slumber.consolidation_schedule.is_empty()`,
   and log `Skipping consolidation: disabled`.
4. Settings editor: add `slumber.consolidation.enabled` (Bool, group "Slumber", label "LLM consolidation",
   help "Merge related memories into summaries. Off by default."). The existing settings tests cover it.
5. `config.example.toml`: `[slumber.consolidation]` gets `# enabled = false` with a one-line reason.
   README: update the Slumber section ("13 phases" → note phase 6 is opt-in).
6. If consolidation is retired: drop Phase 3 and mark the "Wiki pages per realm" TODO item as blocked by it.
   If it stays: fix the scheduler in its own PR instead (wall-clock check every minute, like the dream
   scheduler in Phase 4).

### Task 1.7: Ship

PR (protected types, retirement switch, docs, Task 1.4 test) → CI green → merge → backup-gated deploy.
Verify: the startup and next-slumber logs show `Skipping consolidation: disabled` (if retired); no new summary
appears with a protected `memory_type`.

---

## Phase 1b: Re-check the legacy summaries (PR 2b, script only)

Goal: every summary that predates provenance is either removed (it is fully covered by originals that still
exist, so removing it loses nothing) or flagged for review (it may hold the only copy of something). Nothing is
lost: every removed summary is exported first and can be re-imported.

### Task 1b.1: Script, dry-run by default

**Files:** Create `scripts/recheck_legacy_summaries.py` (stdlib only, same style as
`scripts/purge_cron_transcripts.py`: talks to the API on localhost, API key from `MEMEX8_API_KEY`).

For each memory with `chunk_type == "consolidated"` and no `consolidated_from`, starting with
`profile` and `preference`:

1. **Find candidate originals:** `POST /api/v1/memories/search` with the summary text, `limit 12`,
   `min_score 0.6`. Drop summaries from the results.
2. **Check coverage with one LLM call** (backend and model from env, like `classify_memory_types.py`):
   *"List each distinct fact in SUMMARY. For each, quote the ORIGINAL that states it, or say MISSING.
   Answer as JSON."* Temperature 0.
3. **Classify:**
   - `covered`: every fact is quoted from an original → candidate for archive;
   - `partial`: some facts MISSING → review;
   - `orphan`: no originals found → review (this summary may be the only copy).
4. Write a report to `--out <file>.json` (id, type, class, missing facts, matched original ids) and print counts.
   The report holds memory text, so write it with mode 600 and keep it out of the repo.

Pure-function tests (`scripts/test_recheck_legacy_summaries.py`): `classify()` from a parsed LLM answer
(`all quoted → covered`, `any MISSING → partial`, `no originals → orphan`), and JSON-answer parsing that
tolerates code fences.

Privacy note for the README/script header: this sends summary and original text to the configured LLM backend,
the same exposure consolidation itself had. Supports `LLM_API_URL` for a local model.

### Task 1b.2: Apply (only after reviewing the dry-run report)

Why not just lower importance: slumber's prune "archive" (`importance 0.01`) never deletes, but it doesn't take
a memory out of search either. Importance barely affects search scores, so redundant summaries would keep crowding
the top-k. For `covered` summaries, removal is the useful action; it's safe because every fact is in an original.

`--apply` asks for an explicit `--yes-remove-covered` flag and acts only on `covered` summaries:

1. Take a full cold backup first (`scripts/docker_backup.py backup`), and export each summary
   (`GET /api/v1/memories/{id}`, full payload) to `--backup <file>.jsonl` (mode 600). Write it in the format
   `Engine::import` reads (check `src/engine/mod.rs::import` first, and add a test that round-trips one record),
   so `memex8 import --path <file>` can restore any of them (they're re-embedded).
2. Delete it: `DELETE /api/v1/memories/{id}`.
3. For `partial` and `orphan` summaries, change nothing except adding the tag `legacy-summary-review`, and list them in
   the report for a human to read. If `PATCH` can't add tags, extend the PATCH body to accept `tags_add`, with a test
   in `src/api/routes/memories.rs`. Certified summaries are skipped (the API refuses edits anyway).

Verify: `total_memories` drops by exactly the number of covered summaries; for five of them, a search for one of
their facts still returns the matching original; the review tags match the report counts. Then restart the daemon
so TurboVec drops the removed vectors (startup log count ≈ live count).

---

## Phase 2: Hybrid search, BM25 plus vectors (PR 3)

### Task 2.1: Lexical index module (TDD)

**Files:** Create `src/engine/lexical.rs`; register `pub mod lexical;` in `src/engine/mod.rs`.

Tests first (same file, `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_splits_and_drops_stopwords() {
        assert_eq!(tokenize("The Trip-Grus to Corby 2"), vec!["trip", "grus", "corby", "2"]);
    }

    #[test]
    fn rare_term_outranks_common_terms() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "meeting notes about the project budget");
        idx.upsert("b", "Zephyr router flashed with OpenWrt firmware");
        idx.upsert("c", "project budget meeting again");
        let hits = idx.search("zephyr openwrt", 10);
        assert_eq!(hits.first().map(|h| h.0.as_str()), Some("b"));
    }

    #[test]
    fn upsert_replaces_and_remove_forgets() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "alpha beta");
        idx.upsert("a", "gamma");
        assert!(idx.search("alpha", 5).is_empty());
        assert_eq!(idx.search("gamma", 5).len(), 1);
        idx.remove("a");
        assert!(idx.search("gamma", 5).is_empty());
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn empty_or_stopword_query_returns_nothing() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "alpha");
        assert!(idx.search("", 5).is_empty());
        assert!(idx.search("the and of", 5).is_empty());
    }

    #[test]
    fn results_are_sorted_and_truncated() {
        let mut idx = LexicalIndex::default();
        for i in 0..20 {
            idx.upsert(&format!("m{i}"), &format!("token{} shared", i % 3));
        }
        let hits = idx.search("shared token1", 5);
        assert_eq!(hits.len(), 5);
        assert!(hits.windows(2).all(|w| w[0].1 >= w[1].1));
    }
}
```

Implementation:

```rust
//! In-process BM25 keyword index over memory text.
//!
//! A cache, not a source of truth: it is rebuilt from Qdrant at startup and after
//! slumber, and updated on direct writes. Every hit is re-checked against Qdrant
//! before use, so a stale entry can't surface a deleted memory.
use std::collections::HashMap;

const K1: f32 = 1.2;
const B: f32 = 0.75;
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is",
    "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "were", "will", "with",
];

pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .filter(|t| t.chars().count() > 1 || t.chars().all(|c| c.is_ascii_digit()))
        .filter(|t| !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

#[derive(Default)]
pub struct LexicalIndex {
    postings: HashMap<String, HashMap<String, u32>>, // term -> (memory id -> tf)
    docs: HashMap<String, (Vec<String>, u32)>,       // memory id -> (distinct terms, length)
    total_len: u64,
}

impl LexicalIndex {
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn upsert(&mut self, id: &str, text: &str) {
        self.remove(id);
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return;
        }
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in &tokens {
            *tf.entry(t.clone()).or_default() += 1;
        }
        let len = tokens.len() as u32;
        for (term, n) in &tf {
            self.postings.entry(term.clone()).or_default().insert(id.to_string(), *n);
        }
        self.total_len += len as u64;
        self.docs.insert(id.to_string(), (tf.into_keys().collect(), len));
    }

    pub fn remove(&mut self, id: &str) {
        if let Some((terms, len)) = self.docs.remove(id) {
            self.total_len -= len as u64;
            for term in terms {
                if let Some(p) = self.postings.get_mut(&term) {
                    p.remove(id);
                    if p.is_empty() {
                        self.postings.remove(&term);
                    }
                }
            }
        }
    }

    /// Top `k` (memory id, BM25 score), best first. Ties break on id for determinism.
    pub fn search(&self, query: &str, k: usize) -> Vec<(String, f32)> {
        let n = self.docs.len() as f32;
        if n == 0.0 {
            return Vec::new();
        }
        let avg = self.total_len as f32 / n;
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        let mut scores: HashMap<&str, f32> = HashMap::new();
        for term in &terms {
            let Some(p) = self.postings.get(term) else { continue };
            let df = p.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (id, tf) in p {
                let len = self.docs.get(id).map_or(avg, |d| d.1 as f32);
                let tf = *tf as f32;
                *scores.entry(id.as_str()).or_default() +=
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
            }
        }
        let mut out: Vec<(String, f32)> =
            scores.into_iter().map(|(id, s)| (id.to_string(), s)).collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
        out.truncate(k);
        out
    }
}

/// Text indexed for a memory: heading, content and tags.
pub fn memory_text(m: &crate::storage::qdrant::MemoryPoint) -> String {
    let mut s = String::new();
    if let Some(h) = &m.heading {
        s.push_str(h);
        s.push(' ');
    }
    s.push_str(&m.content);
    for t in &m.tags {
        s.push(' ');
        s.push_str(t);
    }
    s
}
```

Python's `[^0-9a-z]` and Rust's `is_alphanumeric` differ on non-ASCII text. That's fine for the bench (English data),
but note it in `bm25_ref.py`.

Run: `cargo test --release --lib lexical` → 5 passed.

### Task 2.2: Config

**Files:** `src/config.rs` (`SearchConfig`, ~line 30)

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub coverage: CoverageConfig,
    pub hybrid: HybridConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HybridConfig {
    /// Blend BM25 keyword matches into vector search. Off until the bench shows a gain.
    pub enabled: bool,
    /// Bonus for the best keyword match, on the cosine scale (0..=1).
    pub lexical_weight: f32,
    /// Keyword candidates considered per query.
    pub lexical_candidates: usize,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self { enabled: false, lexical_weight: 0.15, lexical_candidates: 50 }
    }
}

impl HybridConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.lexical_weight.is_finite() && (0.0..=1.0).contains(&self.lexical_weight),
            "search.hybrid.lexical_weight must be between 0 and 1"
        );
        anyhow::ensure!(
            (1..=500).contains(&self.lexical_candidates),
            "search.hybrid.lexical_candidates must be between 1 and 500"
        );
        Ok(())
    }
}
```

Call `config.search.hybrid.validate()?` in `AppConfig::from_value` next to the coverage validation.
Tests: defaults validate; weight 1.5 is rejected; candidates 0 is rejected.

Add to the settings editor whitelist (`src/api/settings.rs::FIELDS`, group "Search"):
`search.hybrid.enabled` (Bool) and `search.hybrid.lexical_weight` (Float 0.0–1.0). The existing tests
(`every_field_key_exists_in_the_default_config`, `default_values_pass_their_own_ranges`) cover them.

Document in `config.example.toml`:

```toml
[search.hybrid]
# Blend BM25 keyword matches into vector search (helps names, IDs, rare words).
# enabled = false
# lexical_weight = 0.15
# lexical_candidates = 50
```

### Task 2.3: Score arbitrary IDs against the query vector

**Files:** `src/storage/qdrant.rs` (next to `search_filtered`)

```rust
/// Cosine-score a given set of memory IDs against `vector`, applying the same
/// realm/visibility filter as search. Missing or filtered-out IDs are simply absent.
pub async fn score_ids(
    &self,
    vector: &[f32],
    ids: &[String],
    realm: Option<&str>,
    visibility: Option<&str>,
) -> anyhow::Result<Vec<SearchResult>> {
    let point_ids: Vec<qdrant_client::qdrant::PointId> = ids
        .iter()
        .filter_map(|id| uuid::Uuid::parse_str(id).ok())
        .map(|u| u.to_string().into())
        .collect();
    if point_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut filter = Self::retrieval_filter(realm, None, visibility);
    filter.must.push(Condition::has_id(point_ids.clone()));
    let result = self
        .client
        .search_points(
            SearchPointsBuilder::new(MEMORIES, vector, point_ids.len() as u64)
                .filter(filter)
                .with_payload(true)
                .with_vectors(false),
        )
        .await?;
    Ok(result
        .result
        .into_iter()
        .map(|r| {
            let id = point_id_to_string(r.id.as_ref());
            SearchResult {
                payload: memory_from_payload(&id, &map_to_json(&r.payload)),
                id,
                score: r.score,
            }
        })
        .collect())
}
```

This needs a live Qdrant, so there's no unit test. It gets covered by the bench run and the integration check in Task 2.6.

### Task 2.4: Keep the index in sync

**Files:** `src/engine/mod.rs`

1. Add an Engine field `lexical: Arc<RwLock<lexical::LexicalIndex>>`, initialized empty in `Engine::new`.
2. Add `pub async fn rebuild_lexical_index(&self) -> anyhow::Result<usize>`: `store.scroll_all_memories()`,
   build a fresh index with `memory_text`, swap it in under the write lock, log
   `🔤 Lexical index: N memories`, and return N.
3. Call it at the end of `Engine::new`. On error, log a warning and continue (dense-only).
4. Call it at the end of `trigger_slumber`, after the run (success or failure). Warn on error.
5. Incremental updates:
   - `store_memory_with_certification`: after a successful store, `upsert(id, text)` with the stored text.
   - `edit_memory`: `upsert(id, memory_text(&existing))` after `replace_memory`.
   - `delete_memory`: `remove(id)` after a successful delete.
   - `import` and `restore`: call `rebuild_lexical_index()` at the end.
6. Paths not hooked: dedup, consolidation, unconsolidate and the CLI slumber process. They're covered
   by the post-slumber rebuild or the next restart. Stale entries are harmless because every hit is
   re-checked through `score_ids`.

Unit test (pure): `memory_text` includes the heading, content and tags.

### Task 2.5: Blend into search

**Files:** `src/engine/mod.rs::search_embedded`, after the dense `results` are built and before
`drop(tv_guard)` and the verification re-weighting.

```rust
let hybrid = &self.config.search.hybrid;
let results = if hybrid.enabled && tags.is_none_or(|t| t.is_empty()) {
    let hits = self.lexical.read().await.search(query, hybrid.lexical_candidates);
    blend_lexical(
        results,
        hits,
        hybrid.lexical_weight,
        min_score,
        |missing: Vec<String>| async move {
            self.store
                .score_ids(query_vector, &missing, realm, visibility)
                .await
                .map(|v| v.into_iter().map(|r| (r.score, r.payload)).collect::<Vec<_>>())
        },
    )
    .await?
} else {
    results
};
```

Put the blending logic in a pure function so it can be tested without Qdrant:

```rust
/// Add a keyword bonus to dense candidates and pull in keyword-only candidates.
/// `bonus = weight * bm25 / max_bm25`. Every candidate keeps its true cosine;
/// keyword-only candidates must still reach `min_score` after the bonus.
pub fn apply_lexical_bonus(
    dense: Vec<(f32, MemoryPoint)>,
    lexical_only: Vec<(f32, MemoryPoint)>, // (cosine, point) from score_ids
    hits: &[(String, f32)],                // (id, bm25)
    weight: f32,
    min_score: f32,
) -> Vec<(f32, MemoryPoint)> {
    let max = hits.iter().map(|h| h.1).fold(0.0_f32, f32::max);
    let bonus = |id: &str| -> f32 {
        if max <= 0.0 { return 0.0; }
        hits.iter().find(|h| h.0 == id).map_or(0.0, |h| weight * h.1 / max)
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (s, p) in dense {
        seen.insert(p.id.clone());
        let b = bonus(&p.id);
        out.push((s + b, p));
    }
    for (s, p) in lexical_only {
        if seen.insert(p.id.clone()) {
            let total = s + bonus(&p.id);
            if total >= min_score {
                out.push((total, p));
            }
        }
    }
    out
}
```

The async `blend_lexical` wrapper computes `missing` (hit IDs not in `dense`), calls the closure,
then calls `apply_lexical_bonus`. If the closure fails, log a warning and return `dense` unchanged.

Tests for `apply_lexical_bonus`:
- dense-only input with no hits → scores unchanged;
- a dense item that is also the top hit gets `+weight`;
- a keyword-only item below `min_score` after the bonus is dropped; one above is kept;
- duplicates (an ID in both lists) appear once.

The verification multiplier, intent weighting, sorting and pagination that follow stay unchanged.
Coverage (`coverage_scores`) is computed separately from dense probes and stays unchanged.

### Task 2.6: Bench, then decide

1. Build an image from the branch (`docker build -t memex8:<sha> <deploy dir>`).
2. Bench config for the instance: run once with `[search.hybrid] enabled = false` (should match the Phase 0
   baseline within noise, which checks the harness) and once with `enabled = true`. Try weights 0.10, 0.15 and 0.25.
   Add `--set search.hybrid.enabled=true` style overrides to `instance.sh up` (append a TOML section) instead of
   editing files by hand.
3. **Gate to turn it on:** overall `memex8@15 support_recall` improves by ≥ 5 points and
   `full_witness@15` doesn't drop, on all ten worlds. Pick the best weight.
4. Live smoke test after deploy with hybrid **off**: startup log shows `🔤 Lexical index: N` with N ≈ total memories.
   Then turn it on via Settings → Search (or `settings.toml`) → Restart now.
5. Live check with hybrid on: 10 real queries (proper nouns, IDs, error strings, plus three general
   questions). Compare the top-5 before and after. General questions must not get worse. Keep the queries and
   results out of the repo; store a short summary as a memex8 memory instead, and put only the pass/fail verdict here.

### Task 2.7: Ship (off), then flip the default (on for everyone)

1. PR A (lexical module, config, store method, engine wiring, tests, README "Search" note) ships with
   `enabled = false` → CI → merge → backup-gated deploy → Task 2.6 steps 4–5.
2. PR B, once the gate and the live check pass: change `HybridConfig::default()` to `enabled: true` and
   `lexical_weight` to the bench-chosen value, update `config.example.toml` and the README, and put the bench table
   (overall and per family, before and after) in the PR description. Add a config test asserting the new default.
   Release note: "keyword matching is now on by default; turn it off in Settings → Search".

---

## Phase 3: Mark summaries stale when a source changes (PR 4, conditional)

**Precondition:** Task 1.5 decided to *keep* consolidation, and its scheduler has been fixed. If consolidation
is retired, skip this phase entirely: legacy summaries have no provenance, and no new ones will be made.

### Task 3.1: Find summaries of a source

**Files:** `src/storage/qdrant.rs`

`pub async fn summaries_of(&self, source_id: &str) -> anyhow::Result<Vec<MemoryPoint>>` scrolls
`MEMORIES` with `Filter::must([Condition::matches("consolidated_from", source_id.to_string())])`.
If filtering on this array field needs a payload index, add a keyword index on
`consolidated_from` in `ensure_collections` (idempotent).

### Task 3.2: Mark stale (TDD on the pure part)

**Files:** `src/engine/mod.rs`

`async fn mark_summaries_stale(&self, source_id: &str, reason: &str)`: for each summary returned by
`summaries_of`, `store.update_unprotected_payload` (skips certified) with
`{"verification_status": "stale", "stale_reason": reason, "stale_since": <rfc3339 now>}`.
Make the payload a pure helper `stale_payload(reason, now) -> serde_json::Value` and unit-test it.

Hooks:
- `delete_memory` → reason `"source deleted: <id>"`;
- `edit_memory` → `"source edited: <id>"`;
- slumber verification (`src/engine/slumber.rs`, where `verification_status` is written, ~line 2869):
  when the new status is `contradicted`, reason `"source contradicted: <id>"`. Collect IDs and call once per sweep.
- Not dedup: a dedup winner carries the loser's ancestry, so nothing is lost.

Effect: stale summaries rank at ×0.85 (the existing multiplier) and get re-checked by the verification sweep.
Re-consolidating them automatically is a follow-up.

### Task 3.3: Ship

PR → CI → deploy. Verify on live: edit a throwaway test memory that a summary includes (create both first),
then confirm the summary's `verification_status` turns `stale` with the reason, and delete the test data.

---

## Phase 4: Dreams (PR 5, opt-in)

**What it is:** on a schedule the user picks in Settings, memex8 takes a handful of memories (some long-forgotten,
some recent, some linked by associations), asks the LLM backend to weave them into a short dream, and adds a
one-line "waking note": one *real* connection between the seed memories, citing their IDs. Dreams appear in a
new Dreams tab and through the API.

**Hard rule:** a dream is fiction built from memories, not a memory. Dreams are stored outside the
`memories` collection and are never returned by search, recall, timeline, MCP or Hermes prefetch.
There is no code path from dream generation to `store_memory*`.

**Why it is useful, not just fun:** it resurfaces forgotten memories, and the waking note can point at a link the user
never noticed. It costs one LLM call per dream.

**Privacy:** seed memories are sent to the configured LLM backend, the same exposure consolidation had. The `local`
backend (`LOCAL_LLM_URL`) keeps them on the network. Memories containing the `[private]` marker are never seeds.

### Task 4.1: Config (TDD)

**Files:** `src/config.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DreamConfig {
    /// Off by default.
    pub enabled: bool,
    /// "daily" or "weekly".
    pub frequency: String,
    /// Local hour (0-23) when the dream runs.
    pub hour: u32,
    /// Number of seed memories per dream.
    pub seeds: usize,
    /// Upper bound on the dream's length.
    pub max_words: usize,
    /// Model override; defaults to the consolidation backend's model.
    pub model: Option<String>,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self { enabled: false, frequency: "weekly".into(), hour: 4, seeds: 12, max_words: 350, model: None }
    }
}

impl DreamConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(matches!(self.frequency.as_str(), "daily" | "weekly"), "dream.frequency must be daily or weekly");
        anyhow::ensure!(self.hour <= 23, "dream.hour must be 0-23");
        anyhow::ensure!((4..=40).contains(&self.seeds), "dream.seeds must be 4-40");
        anyhow::ensure!((80..=1500).contains(&self.max_words), "dream.max_words must be 80-1500");
        Ok(())
    }
}
```

Add `pub dream: DreamConfig` (`#[serde(default)]`) to `AppConfig` and call `validate()` in `from_value`.
Tests: defaults are valid and disabled; `frequency = "hourly"` is rejected; `hour = 24` is rejected.

### Task 4.2: Seed selection, a pure function (TDD)

**Files:** Create `src/engine/dream.rs`; add `pub mod dream;` in `src/engine/mod.rs`.

`pub fn pick_seeds(all: &[MemoryPoint], n: usize, now: DateTime<Utc>, rng: &mut impl rand::Rng) -> Vec<MemoryPoint>`

- **Eligible:** non-empty content; not `chunk_type == "consolidated"`; no `cron-finding` tag; content doesn't contain
  `[private]`.
- **Mix:** about half "forgotten" (oldest `last_accessed`, older than 30 days), a quarter "recent" (ingested in the last
  7 days), and a quarter linked to already chosen seeds through `related_memory_ids`. Fill any shortfall from the
  remaining eligible memories at random. No duplicates. Return at most `n`.

Tests (with `rand::rngs::StdRng::seed_from_u64(7)`): ineligible memories are never picked; the result has no
duplicates and at most `n` items; with enough data it contains at least one forgotten and one recent memory; and
with fewer eligible memories than `n` it returns them all.

### Task 4.3: When to dream, a pure function (TDD)

`pub fn dream_due(cfg: &DreamConfig, now_local: NaiveDateTime, last: Option<NaiveDate>) -> bool`

It's true only when `cfg.enabled`, `now_local.hour() == cfg.hour`, and either there's no previous dream or (daily) the
last one was before today or (weekly) it was at least 7 days ago. Tests cover each branch.

This deliberately uses wall-clock checks every minute, not the slumber tick (the cause of the consolidation silence).

### Task 4.4: Dream store on the data volume (TDD)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dream {
    pub id: String,                 // uuid
    pub created_at: String,         // rfc3339
    pub title: String,
    pub dream: String,
    pub waking_note: String,
    pub waking_note_ids: Vec<String>,
    pub seed_ids: Vec<String>,
    pub model: String,
}
```

`DreamStore { dir: PathBuf }` with `save`, `list(limit)` (newest first), `get(id)`, `delete(id)` and
`last_date()`. One JSON file per dream: `<dir>/<YYYYMMDD-HHMMSS>-<id8>.json`, written to a temp file and then
renamed. Directory: next to the TurboVec index (`<index dir>/dreams/`), so dreams are on the data volume and
included in backups. Optionally write a markdown copy to `~/.memex8/dreams/<date>.md` (the host-visible digest
folder), controlled by `memex8_md.enabled`.

Tests use a unique directory under `std::env::temp_dir()`: save/list order, get, delete, `last_date`, and the
fact that a corrupt file is skipped with a warning instead of failing `list`.

### Task 4.5: Generation (TDD on the parsing)

`Engine::dream_now(&self) -> anyhow::Result<Dream>`:

1. `scroll_all_memories()` → `pick_seeds`. Fail with a clear error if there are fewer than 4 eligible memories.
2. Prompt (system): *"You are memex8, dreaming. Weave the numbered memories below into one short, surreal dream
   (at most {max_words} words). Recombine them freely; it's a dream, not a summary. Then write a waking note:
   one real connection between two or more of the memories, stated plainly, without inventing facts. Answer as JSON:
   {"title": ..., "dream": ..., "waking_note": ..., "waking_note_ids": [...]}"*. User message: the seeds as
   `[id] text` lines (trim each to 500 characters).
3. Call the LLM through the same helper `llm_consolidate` uses (`src/engine/slumber.rs`, phase 6), with the
   `dream.model` override.
4. `parse_dream(raw, seeds)`, a pure function: strip code fences; parse the JSON; drop `waking_note_ids` that aren't
   seed IDs; cut `dream` to `max_words`; error on an empty dream.
5. Save through `DreamStore`. Log `🌙 Dream <id8>: <title>`. Never call any `store_memory*`.
6. A mutex prevents two dreams running at once. A second call returns a "dream already in progress" error (HTTP 409).

Tests for `parse_dream`: fenced JSON is accepted; non-seed IDs are dropped; over-long dreams are truncated; garbage
returns an error.

### Task 4.6: Scheduler wiring

**Files:** `src/engine/scheduler.rs`

Add a `tokio::time::interval(Duration::from_secs(60))` branch to the `select!` loop. On each tick, if
`dream_due(&config.dream, Local::now().naive_local(), store.last_date())`, spawn `engine.dream_now()` and log any
error. Config is read from the engine at the time of the tick, so a Settings change applies after the usual restart.

### Task 4.7: API

**Files:** `src/api/routes/dreams.rs` (new) and `src/api/server.rs::api_routes` (behind auth, like everything else).

| Method | Path | Behaviour |
|---|---|---|
| GET | `/api/v1/dreams?limit=20` | newest first; `{dreams: [{id, created_at, title, preview}]}` |
| GET | `/api/v1/dreams/{id}` | full dream |
| POST | `/api/v1/dreams` | dream now (works even when the schedule is off); 409 if one is already running |
| DELETE | `/api/v1/dreams/{id}` | remove one dream |

Add them to the README's REST table.

### Task 4.8: Settings editor choice fields

**Files:** `src/api/settings.rs`, `web-dist/index.html`

Add `Kind::Choice(&'static [&'static str])`. `coerce` accepts only listed strings, and `GET /settings` returns
`"type": "choice", "options": [...]`. The UI renders a `<select>` for it. Then add the fields, group "Dreams":
`dream.enabled` (Bool), `dream.frequency` (Choice daily/weekly), `dream.hour` (Int 0–23), `dream.seeds` (Int 4–40).
Tests: choice accepts a listed value and rejects others; the existing default and range tests cover the new fields.

### Task 4.9: Dreams tab in the UI

**Files:** `web-dist/index.html`

Add a nav tab "Dreams" next to Timeline. It shows a list of cards (date, title, first two lines) and a **Dream now**
button (POST; shows "dreaming…" until it returns). Clicking a card opens a modal with the dream, the waking note
and "woven from" chips for each seed. A chip opens the existing memory detail view, and seeds that no longer exist
show as greyed out. There's also a delete button with a confirmation. The styling matches the HUD (violet/cyan accent,
italic serif for the dream text). Verify in a browser per `references/frontend-patterns.md`.

### Task 4.10: Ship and verify

PR → CI → backup-gated deploy. On live: Settings → Dreams → enable; then **Dream now**. Check:
- a dream file appears in the data volume and the tab shows it;
- every `waking_note_ids` entry opens a real memory;
- **quarantine check:** search, using the API, for a distinctive phrase from the dream text: it returns no dream content,
  and `total_memories` is unchanged;
- with the schedule set to the next hour, a dream appears once and only once in that hour.

### Task 4.11: Review flow: dreams wait for the user to keep or discard them

Decided 2026-11-13: a new dream is **pending**. At a time the user chooses, the agent shows it and asks whether to
keep it.

1. Add `status: "pending" | "kept"` to `Dream` (default `pending`; files without the field load as `kept`).
   `POST /api/v1/dreams/{id}/keep` marks it kept. Discarding is the existing `DELETE`. `GET /api/v1/dreams?status=pending`
   filters. Pending dreams older than 14 days are deleted at the next dream run (configurable `dream.pending_days`).
   Tests: status round-trip, legacy file → kept, expiry.
2. **Dreams tab:** pending dreams show **Keep** and **Discard** buttons and a "new" badge on the tab.
3. **Optional "also remember it":** `POST /api/v1/dreams/{id}/keep` with `{"remember": true}` also stores the dream as a
   memory with `memory_type: "dream"`, tag `dream`, and `[dream]` prefixed to its content. Search, recall and Hermes
   prefetch exclude `memory_type == "dream"` unless the request passes `include_dreams: true`, so the quarantine rule still
   holds. Test: a stored dream memory is absent from default search and present with `include_dreams`.
4. **Agent prompt at a chosen time (only if the user asks for it):** the Hermes plugin gets a `memex8_dreams` tool with
   `action = pending | keep | discard` (plus `remember` for keep). A Hermes cron job, created only on request, runs at
   the user's chosen time. It calls `memex8_dreams(action="pending")`; if there's a dream, it shows it to the user with the
   waking note and asks "Keep this dream?". The user's reply maps to `keep`/`discard`. If there are no pending dreams,
   the job stays silent (`[SILENT]`). The job's prompt and schedule live in Hermes, not memex8.
   Plugin tests: tool schema, action routing, and an error message when the server has no dreams API.
5. Settings, group "Dreams": the generation time (`dream.hour`) stays in memex8. The delivery time belongs to the
   Hermes cron job, so the Settings help text says so.

Verify: generate one dream, then run the cron job by hand. The message arrives with the dream; answering "keep" marks it
kept (tab badge clears); a second dream answered "discard" disappears from the tab and the data folder.

---

## Phase 5: Deferred (design notes only)

- **Superseded facts (paper L4/Evolution).** When a new memory nearly duplicates an older one on the
  same subject but with a different value, store `supersedes: <old id>` on the new one and
  `superseded_by` on the old one. Rank superseded memories ×0.5. The main risk is false links; it needs its own bench
  measure (`obsolete_hit@15` from Phase 0 is that measure).
- **Multi-query search (paper L1).** `POST /memories/search` accepts `queries: [..]`; run each, merge by
  best score, dedupe. Most of the gain needs the agent (Hermes plugin) to split questions into their inputs,
  so pair it with a plugin change.
- **Better memory typing.** Run `scripts/classify_memory_types.py --dry-run` to see how many `general`
  memories would become `instruction`, `decision` or `preference`. More accurate types make Phase 1's protection
  cover real rules.

---

## Rollout summary

| PR | Contents | Deploy needed | Verify |
|---|---|---|---|
| 1 | bench harness, baseline numbers, this plan | no (repo only) | sanity gate (Task 0.7) |
| 2 | protected types, consolidation switch, diagnosis test | yes | CI; logs show phase 6 skipped if retired |
| 2b | legacy summary re-check script; dry run, then apply | script run on live | counts, five spot checks, TurboVec reload |
| 3A | lexical index + hybrid (default off) | yes | startup log, bench gate, live A/B queries |
| 3B | hybrid default on for everyone | yes | config default test; bench table in PR |
| 4 | stale summaries (only if consolidation is kept) | yes | live edit test |
| 5 | dreams (opt-in) | yes | dream now, quarantine check, schedule fires once |

Every deploy: slumber idle check, cold backup via `scripts/docker_backup.py upgrade`, memory count
equal or higher afterwards, login and search still working.

## Risks and tradeoffs

- **Index drift.** Hooks miss some writers (dedup, consolidation, CLI slumber). Mitigation: rebuild after
  every daemon slumber and at startup, and re-check every hit in Qdrant. The worst case is a missing keyword boost,
  never a wrong or deleted result.
- **Startup time and memory.** Scrolling about 6k payloads at startup takes a few seconds, and the index is a few MB.
  If the corpus grows past about 100k, revisit (persist the index or move to Qdrant sparse vectors).
- **Score semantics.** The bonus can lift a weak-cosine keyword match above a strong semantic one.
  The bench weight sweep and live A/B queries bound this, and the weight is tunable in Settings.
- **Bench vs real use.** MemDerive is workflow data (travel, shopping, degrees), not personal chat memory.
  Treat it as a regression gauge for gathering, and back it with the live A/B queries.
- **Protected types never consolidate.** Long-lived memories can accumulate near-duplicates. Dedup still
  applies to them (long-lived types only dedup within the same type).
- **Legacy summary removal.** The LLM coverage check could call a summary `covered` when an original only
  paraphrases part of it. Mitigations: dry run first with a human-readable report, an explicit flag for removal,
  a cold backup, an importable export, and five spot checks after applying.
- **Dreams read as facts.** A dream could quote a memory closely enough to look like one. That's why dreams never
  enter `memories`, and the quarantine check is part of shipping. The waking note may only cite seed IDs.

## Decisions log

| Date | Decision |
|---|---|
| 2026-11-13 | Re-check legacy summaries; remove only fully covered ones, flag the rest (Phase 1b). |
| 2026-11-13 | Retire consolidation unless it meets the Task 1.5 criteria. |
| 2026-11-13 | Hybrid search becomes the default for everyone after the gate (Task 2.7, PR B). |
| 2026-11-13 | Add opt-in dreams, quarantined from factual memory (Phase 4). |
| 2026-11-13 | Consolidation gets an on/off switch in Settings (Task 1.6). |
| 2026-11-13 | New dreams wait as "pending"; on request, a Hermes cron job shows the dream at a chosen time and asks whether to keep it. Keeping can optionally store it as a `dream` memory, excluded from default search (Task 4.11). |
| 2026-11-13 | This plan stays free of personal data. Instance-specific numbers and queries go in the memex8 store, not the repo. |

## Findings

_(Aggregate numbers only; nothing that identifies a person.)_

**Phase 0 baseline (2026-11-13), 10 worlds, 1,503 questions, top-15:**

| system | support recall | witness recall | full witness | obsolete hit |
|---|---:|---:|---:|---:|
| memex8 vector-only | 47.61 | 50.15 | 31.94 | 31.74 |
| BM25 reference | 47.81 | 47.59 | 29.01 | 25.22 |

At top-50, memex8 scores 66.65 support recall and BM25 62.11. Sanity gate: passed (BM25 ≈ vectors overall; each wins
different worlds, which suggests blending them).

**Task 1.4 verdict:** confirmed. The scheduler unit test shows that with hourly ticks only 11 of 60 start minutes ever
hit 03:00 ± 5. A production daemon started at :25 past the hour, so its ticks can never reach the window. Consolidation
is now opt-in (Phase 1, PR #49).

**Phase 2 weight sweep (hybrid on, same harness):**

| lexical_weight | support recall @15 | full witness @15 | obsolete hit @15 | support recall @50 |
|---|---:|---:|---:|---:|
| 0.10 | 51.26 | 33.20 | 33.13 | 68.97 |
| 0.15 | 52.46 | 34.00 | 32.73 | 69.26 |
| 0.25 | 52.78 | 33.60 | 31.74 | 68.80 |

Every world improves at 0.15 and 0.25; none regresses; full witness never drops. By family (support @15, baseline →
0.15): temporal 75.3 → 84.1, semantic 39.5 → 46.4, rules 51.2 → 55.4, set/quant 32.9 → 37.4, closure 62.4 → 62.2
(0.25: 61.1). The gate (+5 points support, no full-witness drop) is met by 0.25 (+5.17). 0.15 (+4.85) has the
best full-witness score and the smallest closure dip; the two are within a fraction of a point.

**Pending:** Task 1.5 numbers (after Phase 1b), Phase 1b counts, live A/B verdict, final weight (PR B).
