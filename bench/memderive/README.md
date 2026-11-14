# MemDerive retrieval bench

Scores how well memex8 *gathers* the memories a question needs, using the public
[DeriveMem](https://anonymous.4open.science/r/DeriveMem-67C6) data from
*MemDerive: A Diagnostic Benchmark for Agent Memory over Evolving Data* (arXiv:2610.25373).
No LLM judge and no answer model: each question's gold says which events support the answer,
so the bench checks whether memex8's top-k contains them. Scores are deterministic and the
only cost is embeddings (about one cent per full run).

## Run

```bash
python3 bench/memderive/fetch.py                       # 10 worlds, 4,692 events, 1,503 questions
MEMEX8_BENCH_ENV=/path/to/.env.memex8 \
  python3 bench/memderive/run.py --image memex8:<tag> --label baseline
# A/B a config change:
  python3 bench/memderive/run.py --image memex8:<tag> --label hybrid --extra-toml bench/memderive/configs/hybrid-0.15.toml
```

`MEMEX8_BENCH_ENV` supplies the embedding settings (`OPENAI_API_KEY`, `EMBEDDING_*`); any `MEMEX8_*`
lines are dropped and a random bench key is generated. Each world runs in a fresh, isolated
instance (`instance.sh`: compose project `memex8bench`, port 127.0.0.1:18080, own volumes, slumber off).
Results go to `results/` (git-ignored).

## Metrics (top-k, k = 15 and 50)

| metric | meaning |
|---|---|
| support recall | share of the gold support events retrieved |
| witness recall | best coverage of any minimal sufficient event set |
| full witness | a complete witness was retrieved (answerable from the context alone) |
| obsolete hit | a superseded event was retrieved |

`bm25@k` is an event-level BM25 reference over the same texts, the paper's strongest baseline.

Unit tests: `cd bench/memderive && python3 -m unittest -q test_metrics`.
Data is CC BY 4.0 (DeriveMem); the bench code is MIT like the rest of memex8.
