# PLAN — Fix TurboVec persistence path + MEMEX8.md write-back targets

> Historical: both fixes described here have since landed in main (TurboVec path env overrides, MEMEX8.md output_dir).

Date: 2026-10-22
Branch: `fix/turbovec-and-memex8-md-paths`

## Diagnosis

Both defects were confirmed against the live container (`memex8-memex8-1`, up since 2026-10-20,
image `memex8-memex8`, 6 restarts) and the live Qdrant store (12,359 points / 12,061 indexed vectors).

### Bug 1 — TurboVec index never persists (2,691 failed saves since 2026-10-05)

Log signature (repeats every ~20 min, once per slumber cycle):

```
WARN memex8::engine::slumber: Failed to save TurboVec index: No such file or directory (os error 2)
```

Root cause chain:

1. `config.toml` `[turbovec]` sets `index_path = "data/memories.tv"` and
   `id_map_path = "data/memories_ids.json"` — **explicit, relative** paths.
2. `src/config.rs` declares these fields with
   `#[serde(default = "default_turbovec_index_path")]`. The default fn reads
   `TURBOVEC_INDEX_PATH`, but **serde only calls it when the key is absent from the
   TOML**. Because `config.toml` sets the key explicitly, the env var is never consulted.
3. `docker-compose.yml` exports the correct absolute paths
   (`TURBOVEC_INDEX_PATH=/var/lib/memex8/data/memories.tv`), but they are dead.
4. The container's cwd is `/` and it runs as uid 1000, so the relative path resolves to
   `/data/memories.tv`. `create_dir_all("/data")` fails (root-owned `/`, error swallowed by
   `let _ =` at `src/engine/slumber.rs:304`), then the write fails with ENOENT (os error 2).

Consequence: `/var/lib/memex8/data/memories.tv` and `memories_ids.json` are frozen at
**2026-10-04 09:57** — 18 days stale. Every cycle re-quantizes all ~12.3k vectors and
throws the result away. The same broken path also breaks `init_turbovec_index`
(`src/engine/mod.rs:182`), so the on-disk index is never loaded at startup either.

Impact is **performance, not correctness**: Qdrant still serves search, and results are
accurate. The cost is a full re-quantization of 12,357 vectors every cycle plus a
permanently stale companion `memories_ids.json`.

### Bug 2 — MEMEX8.md phase writes nothing (`md=0` every cycle)

Log signature:

```
WARN memex8::engine::slumber: Failed to write "MEMEX8.md": Permission denied (os error 13)
WARN memex8::engine::slumber: Failed to write "https://youtu.be/MEMEX8.md": No such file or directory (os error 2)
```

Root cause: `update_memex8_md` (`src/engine/slumber.rs:1627-1666`) groups memories by
`Path::new(source_file).parent()` and writes `MEMEX8.md` into that directory. But in this
deployment `source_file` is **not a filesystem path** — it is the plugin's `source` label.
Live distribution in Qdrant:

| source_file | count |
|---|---|
| `hermes` | 990+ |
| `hermes-sync` | 949+ |
| `""` (empty) | 42+ |
| `hermes-tool` | 10+ |
| `manual` | 8 |
| `general` | 1 |
| URL-form (e.g. `https://youtu.be/...`) | some |

Consequences of the label-as-path assumption:

- `"hermes"` → `parent()` = `""` → target `"MEMEX8.md"` (relative) → resolves against cwd `/`
  → **permission denied** as uid 1000.
- `"https://youtu.be/xyz"` → `parent()` = `"https://youtu.be"` → target is a bogus URL-shaped
  path → **ENOENT**.
- `""` / `None` → `parent()` = `None` → skipped entirely.

Net: the phase logs two warnings per cycle, writes **zero** files, and `report.memex8_md_written`
is always 0. The feature has never worked in this deployment.

## Fixes

### Fix 1 — env var wins over config file for TurboVec paths

In `src/config.rs::AppConfig::load`, after deserializing, override `turbovec.index_path` and
`turbovec.id_map_path` from `TURBOVEC_INDEX_PATH` / `TURBOVEC_ID_MAP_PATH` when those env vars
are set.

Precedence becomes: **env var → config.toml → built-in default**. That is the correct shape for
a deployment-specific path: the compose file already exports the right absolute path on the
persistent `memex8_memex8_data` volume; the config file's relative path becomes a fallback for
non-Docker/native runs.

This is a single resolution point, so it fixes **both** consumers at once:
`init_turbovec_index` (engine/mod.rs:182) and the phase-2 save (engine/slumber.rs:292).

No change to `config.toml` needed — the env var path is already correct there.

### Fix 2 — MEMEX8.md write-back targets real directories

New config knob `[memex8_md] output_dir` (default `/watch`), with `#[serde(default)]` so
existing configs without the key still load.

New resolution for each memory's target directory:

1. `source_file` is a **real path** whose parent exists → write `MEMEX8.md` next to it
   (preserves the original "context file beside the content" feature; inside the container the
   watched path *is* `/watch/...`, so this lands on the host at
   `myprojs/memex8/watch_dirs/...`).
2. `source_file` is a **label** (no path separators) or a **URL** → sanitize it to a safe
   directory name and write to `<output_dir>/<label>/MEMEX8.md`.
3. `source_file` is `None` or empty → skipped (no meaningful grouping key).

Label sanitization: replace any character outside `[A-Za-z0-9._-]` with `-`, collapse repeats,
strip leading/trailing `-`, cap length. Guarantees no path traversal and no bogus URL-shaped
paths.

Also: create the target directory when missing (only under `output_dir` for the label case),
and stop attempting writes whose parent directory does not exist.

## Files to change

| File | Change |
|---|---|
| `src/config.rs` | Env-var override for turbovec paths in `AppConfig::load`; add `output_dir` to `Memex8MdConfig` (+ serde default fn, + `AppConfig::default()` literal) |
| `src/engine/slumber.rs` | Rework `update_memex8_md` target resolution |
| `config.example.toml` | Document `[memex8_md] output_dir` |

## Verification plan

1. `cargo build` → 0 errors; `cargo fmt`; `cargo test` (skip `test_kmeans_simple`, known
   non-deterministic on tiny fixture).
2. Snapshot image for rollback: `docker tag memex8-memex8 memex8-memex8:pre-<sha>-<date>`.
3. `docker compose build --no-cache memex8` then
   `docker compose up -d --force-recreate --no-deps memex8` (run with `background=true`).
4. Confirm via md5sum that the container binary matches the host build.
5. Trigger slumber: `docker exec memex8-memex8-1 memex8 slumber trigger`.
6. Assert logs contain `Saved TurboVec index: N vectors ... /var/lib/memex8/data/memories.tv`
   and `Wrote N MEMEX8.md files` with N > 0 — and **no** `Failed to save TurboVec index`
   or `Failed to write` warnings.
7. Assert `memories.tv` mtime is now (not 2026-10-04) and that MEMEX8.md files appear on the
   host under `myprojs/memex8/watch_dirs/`.
8. Re-verify memex8 health end-to-end: `memex8_search` / `memex8_recall` still return
   correct content; `total_memories` unchanged (no data loss).

## Rollback

`docker tag memex8-memex8:pre-<sha>-<date> memex8-memex8 && docker compose up -d --force-recreate --no-deps memex8`
