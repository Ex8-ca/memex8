# TurboVec Migration Plan — memex8 Full Replacement (Option B)

**Date:** 2026-05-24
**Target:** Replace memex8's custom ScalarQuant + Qdrant `quantized` collection with `turbovec` Rust crate
**Goal:** All vector storage/search handled by TurboVec. Qdrant only stores payload/metadata.

---

## Summary

TurboVec wraps Google's TurboQuant algorithm — a data-oblivious quantizer that needs zero training, matches the Shannon lower bound on distortion, and achieves 16x compression at 2-bit. It's available as a Rust crate (`turbovec`) and Python bindings.

**Key performance claims:**
- 10M vectors: 31 GB → 4 GB (8x at 4-bit)
- 12–20% faster than FAISS FastScan on ARM
- Zero codebook training, no rebuilds as corpus grows
- SIMD: NEON (ARM) + AVX-512BW (x86)

---

## Files to Change

### 1. `Cargo.toml` — Add dependency

```toml
[dependencies]
turbovec = "0.1"
```

Remove any now-unused deps (nothing specific — keep `ndarray` for misc math).

### 2. `src/engine/quantizer.rs` — Full rewrite (~400 → ~80 lines)

**Remove entirely:**
- `ScalarQuantizer` struct (per-coordinate min/max tracking)
- `QuantizedVector` struct
- `quantize()` method (min/max scaling + bit-pack)
- `dequantize()` method
- `quality_report()` method
- `pack_bits()` / `unpack_bits()` helper functions
- `find_min_max()` / `compute_per_dim_ranges()`

**Replace with:**
```rust
use turbovec::TurboQuantIndex;
use anyhow::Result;

pub struct TurboQuantVectorIndex {
    index: TurboQuantIndex,
    dim: usize,
    bit_width: usize,
    vector_count: usize,
    /// Maps internal turbovec index → memex8 memory id
    id_map: Vec<String>,
}

impl TurboQuantVectorIndex {
    pub fn new(dim: usize, bit_width: usize) -> Self {
        Self {
            index: TurboQuantIndex::new(dim, bit_width),
            dim,
            bit_width,
            vector_count: 0,
            id_map: Vec::new(),
        }
    }

    /// Batch-add vectors during slumber. Returns number added.
    pub fn add_batch(&mut self, ids: &[String], vectors: &[Vec<f32>]) -> usize {
        let count = ids.len().min(vectors.len());
        self.index.add(vectors);
        self.id_map.extend_from_slice(&ids[..count]);
        self.vector_count += count;
        count
    }

    /// Search returns (scores, indices into id_map).
    pub fn search(&self, query: &[f32], k: usize) -> (Vec<f32>, Vec<usize>) {
        let (scores, raw_indices) = self.index.search(query, k);
        let indices: Vec<usize> = raw_indices.iter()
            .map(|&i| i as usize)
            .collect();
        (scores, indices)
    }

    /// Get memory IDs for search results (looks up id_map by turbovec indices).
    pub fn resolve_ids(&self, indices: &[usize]) -> Vec<&str> {
        indices.iter()
            .filter_map(|&i| self.id_map.get(i).map(|s| s.as_str()))
            .collect()
    }

    /// Persist to disk.
    pub fn save(&self, path: &str) -> Result<()> {
        self.index.write(path).map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Load from disk.
    pub fn load(path: &str, dim: usize, bit_width: usize) -> Result<Self> {
        let index = TurboQuantIndex::load(path)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        // NOTE: id_map is NOT persisted by turbovec — we need a companion file
        Ok(Self {
            index,
            dim,
            bit_width,
            vector_count: 0,  // reload from companion
            id_map: Vec::new(), // reload from companion
        })
    }

    pub fn vector_count(&self) -> usize { self.vector_count }
    pub fn bit_width(&self) -> usize { self.bit_width }
    pub fn dimensions(&self) -> usize { self.dim }

    /// Compression ratio (float32 → bit_width).
    pub fn compression_ratio(&self) -> f64 {
        32.0 / self.bit_width as f64
    }
}
```

### 3. `src/engine/slumber.rs` — Pipeline changes

**Current flow (to replace):**
```
fetch_all_vectors() → quantizer.quantize(&v) → upsert to Qdrant "quantized" collection
```

**New flow:**
```
fetch_all_vectors_with_ids() → index.add_batch(&ids, &vectors) → index.save("data/memories.tv")
```

**Specific line changes:**

- **Line 291:** `let reconstructed = quantizer.dequantize(&qv)` 
  → Remove — TurboVec handles quality internally. If quality metrics needed, use `index.search(&vec, 1)` and compare cosine distance.

- **Line 396:** Same dequantize usage → Remove.

- **Qdrant `quantized` collection references:** Remove all. This collection is no longer used.

- **Startup/init:** After slumber pipeline init, call `TurboQuantVectorIndex::load("data/memories.tv")` or create fresh.

### 4. `src/storage/qdrant.rs` — Remove quantized collection usage

- Remove any references to `config.collection_quantized`
- Remove `SearchPointsBuilder` calls targeting the quantized collection
- Search now works: TurboVec → get memory IDs → fetch payload from Qdrant `memories` collection

### 5. `config.example.toml` — Config changes

```toml
# REMOVE or deprecate:
# [quantizer]
# algorithm = "scalar"
# bit_width = 3.5
# normalize_input = true

# ADD:
[turbovec]
# Bit width for TurboQuant compression (2 or 4)
bit_width = 4
# Path to persist the index
index_path = "data/memories.tv"
# Companion file for id_map (turbovec doesn't persist custom IDs)
id_map_path = "data/memories_ids.json"

# REMOVE:
# collection_quantized = "quantized"
```

### 6. `src/main.rs` or `src/config.rs` — Config struct changes

- Add `TurbovecConfig` struct with `bit_width`, `index_path`, `id_map_path`
- Remove `QuantizerConfig` if it exists
- Remove `collection_quantized` from collection names config

### 7. Search path update

Wherever search currently queries the `quantized` collection:

```rust
// OLD: Query Qdrant quantized collection
let results = client.search_points(
    SearchPointsBuilder::new(config.collection_quantized, query_vector, limit)
).await?;

// NEW: Query turbovec, then fetch payload from Qdrant
let (scores, indices) = index.search(&query_vector, limit);
let ids: Vec<&str> = index.resolve_ids(&indices);
let payloads = fetch_memories_by_ids(&client, &config.collection_memories, &ids).await?;
```

### 8. Initial index build

On first startup (no `memories.tv` file exists):
- Fetch all vectors + IDs from Qdrant `memories` collection
- Build fresh `TurboQuantVectorIndex`
- `index.add_batch(&ids, &vectors)` 
- `index.save("data/memories.tv")`
- Save id_map to `data/memories_ids.json`

---

## Design Decisions

### ID Mapping
TurboVec uses sequential internal indices (0, 1, 2, ...). memex8 uses UUIDs. We need a companion file (`memories_ids.json`) — a simple `Vec<String>` or `HashMap<usize, String>` — that maps turbovec internal indices → memex8 UUIDs.

**Why not `IdMapIndex`?** TurboVec has `IdMapIndex` for stable uint64 IDs, but memex8 uses string UUIDs. Two options:
1. Hash UUIDs to u64 (risk: collisions on large datasets)
2. Maintain own id_map Vec (chosen — simple, collision-free, minimal overhead)

### Rebuild on Add/Delete
TurboQuant is data-oblivious (no training), but `add()` appends to the internal array — it doesn't support random deletes. For memex8:
- **Adds:** Append to index live (TurboQuant supports incremental add), rebuild id_map
- **Deletes:** Mark as deleted in Qdrant, rebuild full index during next slumber
- **Slumber:** Full rebuild from Qdrant (ensures consistency)

---

## Files Summary

| File | Action | LOC change |
|---|---|---|
| `Cargo.toml` | Add `turbovec` dep | +1 line |
| `src/engine/quantizer.rs` | Full rewrite | ~400→80 lines |
| `src/engine/slumber.rs` | Remove quantized collection upsert, add turbovec save | ~-30, +20 lines |
| `src/storage/qdrant.rs` | Remove quantized collection references | ~-15 lines |
| `src/config.rs` or `src/main.rs` | Add TurbovecConfig, remove quantizer config | ~+15, -10 lines |
| `config.example.toml` | Replace [quantizer] with [turbovec] | ~+10, -8 lines |
| `ARCHITECTURE.md` | Update docs | ~-20, +20 lines |
| `TODO.md` | Mark done | ~-5 lines |

**Net:** ~500 lines removed, ~150 added.

---

## Testing

After implementation, verify with:

```bash
# Build
cargo build --release

# Run memex8
./target/release/memex8 serve

# Ingest some memories
memex8 ingest ~/.hermes/memex8/

# Trigger slumber
memex8 slumber trigger

# Check the index file was created
ls -lh data/memories.tv

# Search should work
memex8 search "test query" --limit 5
```

---

## Risks & Mitigation

| Risk | Mitigation |
|---|---|
| turbovec API changes pre-1.0 | Pin version in Cargo.toml |
| Index file corruption on crash | Rebuild from Qdrant during next slumber (vectors are the source of truth) |
| Memory usage spike during full rebuild | Already happens with current ScalarQuant; turbovec is more memory-efficient |
| id_map desync on crash | Always rebuild id_map from Qdrant on startup alongside index rebuild |
