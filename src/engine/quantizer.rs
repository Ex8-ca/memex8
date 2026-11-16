//! TurboVec-backed vector index for memex8.
//!
//! Replaces the old AdaptiveScalarQuantizer. All vector storage and search
//! is handled by the `turbovec` crate (TurboQuant algorithm). Qdrant stores
//! only payload/metadata.

use anyhow::{Context, Result};
use turbovec::{SearchResults, TurboQuantIndex};

/// TurboQuant-compressed vector index with string UUID mapping.
///
/// TurboVec uses sequential internal indices (0, 1, 2, …). memex8 uses UUIDs,
/// so we maintain a companion `id_map` Vec that maps turbovec internal indices
/// → memex8 memory UUIDs.
pub struct TurboQuantVectorIndex {
    index: TurboQuantIndex,
    dim: usize,
    bit_width: usize,
    vector_count: usize,
    /// Maps internal turbovec slot index → memex8 memory UUID.
    id_map: Vec<String>,
}

impl TurboQuantVectorIndex {
    /// Create a fresh index. `bit_width` must be 2 or 4.
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
    /// Vectors must all have length == self.dim.
    pub fn add_batch(&mut self, ids: &[String], vectors: &[Vec<f32>]) -> usize {
        let count = ids.len().min(vectors.len());
        if count == 0 {
            return 0;
        }
        // turbovec::add takes a flat &[f32] of length n * dim
        let flat: Vec<f32> = vectors[..count].iter().flatten().copied().collect();
        self.index.add(&flat);
        self.id_map.extend_from_slice(&ids[..count]);
        self.vector_count += count;
        count
    }

    /// Search: returns (scores, internal slot indices) for a single query.
    pub fn search(&self, query: &[f32], k: usize) -> (Vec<f32>, Vec<usize>) {
        if self.vector_count == 0 {
            return (vec![], vec![]);
        }
        let results: SearchResults = self.index.search(query, k);
        let indices: Vec<usize> = results.indices.iter().map(|&i| i as usize).collect();
        (results.scores, indices)
    }

    /// Resolve internal slot indices → memex8 UUIDs.
    pub fn resolve_ids(&self, indices: &[usize]) -> Vec<&str> {
        indices
            .iter()
            .filter_map(|&i| self.id_map.get(i).map(|s| s.as_str()))
            .collect()
    }

    /// Persist index + companion id_map to disk.
    pub fn save(&self, index_path: &str, id_map_path: &str) -> Result<()> {
        self.index.write(index_path)?;
        // Save id_map as JSON companion file
        if let Some(parent) = std::path::Path::new(id_map_path).parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory for {}", id_map_path)
            })?;
        }
        let json = serde_json::to_string(&self.id_map)?;
        std::fs::write(id_map_path, json)
            .with_context(|| format!("Failed to write id_map to {}", id_map_path))?;
        Ok(())
    }

    /// Load index + companion id_map from disk.
    ///
    /// The two files are renamed into place one after the other, so a crash can
    /// leave a new `.tv` beside an old id map (or a foreign `.tv` beside a
    /// deliberately matching map). The slot count and the map length must agree
    /// or the pair is refused: a wrong slot→UUID mapping returns the wrong
    /// memory for a score, which no later check can undo.
    pub fn load(index_path: &str, id_map_path: &str, dim: usize, bit_width: usize) -> Result<Self> {
        let index = TurboQuantIndex::load(index_path)
            .with_context(|| format!("Failed to load TurboVec index from {}", index_path))?;
        let id_map_json = std::fs::read_to_string(id_map_path)
            .with_context(|| format!("Failed to read id_map from {}", id_map_path))?;
        let id_map: Vec<String> = serde_json::from_str(&id_map_json)
            .with_context(|| format!("Failed to parse id_map from {}", id_map_path))?;
        if index.len() != id_map.len() {
            anyhow::bail!(
                "TurboVec index {} holds {} vectors but its id map {} lists {} IDs: \
                 refusing a mixed pair",
                index_path,
                index.len(),
                id_map_path,
                id_map.len()
            );
        }
        Ok(Self {
            vector_count: index.len(),
            index,
            dim,
            bit_width,
            id_map,
        })
    }

    pub fn vector_count(&self) -> usize {
        self.vector_count
    }
    /// Number of vectors actually present in the compressed file itself, as
    /// opposed to the id map that is stored beside it.
    pub fn slots(&self) -> usize {
        self.index.len()
    }
    /// Memory IDs this index can currently answer for, in slot order.
    ///
    /// Used to prove a persisted index matches the live corpus by ID set rather
    /// than by count: consolidation swaps deleted sources for summaries, so the
    /// count can match while every affected ID is wrong.
    pub fn ids(&self) -> &[String] {
        &self.id_map
    }
    pub fn bit_width(&self) -> usize {
        self.bit_width
    }
    pub fn dimensions(&self) -> usize {
        self.dim
    }

    /// Compression ratio (float32 → bit_width).
    pub fn compression_ratio(&self) -> f64 {
        32.0 / self.bit_width as f64
    }
}

// ─── Bit width decision (kept for config compatibility) ──────────────────────

/// Decide the optimal bit width for a memory based on its access patterns and importance.
///
/// TurboVec only supports bit_width ∈ {2, 4}. We map the old fractional values
/// to the nearest supported width:
/// - 2.0 → 2
/// - 2.5, 3.0, 3.5 → 4
/// - 4.0 → 4
/// - unquantized (None) → skip (store full precision in Qdrant only)
pub fn decide_bit_width(access_count: u64, importance: f64) -> Option<usize> {
    let old_bw = decide_bit_width_legacy(access_count, importance);
    match old_bw {
        None => None, // unquantized
        Some(bw) if bw <= 2.0 => Some(2),
        Some(_) => Some(4),
    }
}

/// Legacy decision logic — kept for reference and config compatibility.
fn decide_bit_width_legacy(access_count: u64, importance: f64) -> Option<f32> {
    if access_count >= 50 || importance >= 0.95 {
        return None;
    }
    if access_count >= 20 || importance >= 0.8 {
        return Some(4.0);
    }
    if access_count == 0 && importance < 0.3 {
        return Some(2.0);
    }
    if access_count < 5 && importance < 0.5 {
        return Some(2.5);
    }
    Some(3.5)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    fn make_vector(dims: usize, seed: u64) -> Vec<f32> {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let v: Vec<f32> = (0..dims).map(|_| rng.gen()).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    #[test]
    fn test_add_search_roundtrip() {
        let dims = 128;
        let mut index = TurboQuantVectorIndex::new(dims, 4);

        let ids: Vec<String> = (0..10).map(|i| format!("mem-{:04}", i)).collect();
        let vectors: Vec<Vec<f32>> = (0..10).map(|i| make_vector(dims, i)).collect();

        let added = index.add_batch(&ids, &vectors);
        assert_eq!(added, 10);
        assert_eq!(index.vector_count(), 10);

        // Search with one of the added vectors
        let query = &vectors[3];
        let (scores, indices) = index.search(query, 5);
        assert_eq!(scores.len(), 5);
        assert_eq!(indices.len(), 5);

        let resolved = index.resolve_ids(&indices);
        assert_eq!(resolved.len(), 5);
        // The query vector itself should be the top result (cosine distance = highest score)
        assert!(
            resolved.iter().any(|id| id.starts_with("mem-0003")),
            "Query vector not in top 5 results. Got: {:?}",
            resolved
        );
    }

    #[test]
    fn test_save_load() {
        let dims = 64;
        let tmp = std::env::temp_dir();
        let index_path = tmp.join("test_index.tv").to_string_lossy().to_string();
        let id_map_path = tmp.join("test_ids.json").to_string_lossy().to_string();

        {
            let mut index = TurboQuantVectorIndex::new(dims, 4);
            let ids: Vec<String> = (0..5).map(|i| format!("test-{}", i)).collect();
            let vectors: Vec<Vec<f32>> = (0..5).map(|i| make_vector(dims, i)).collect();
            index.add_batch(&ids, &vectors);
            index.save(&index_path, &id_map_path).unwrap();
        }

        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, dims, 4).unwrap();
        assert_eq!(loaded.vector_count(), 5);
        assert_eq!(loaded.slots(), 5);
        assert_eq!(loaded.id_map.len(), 5);
        assert_eq!(loaded.id_map[0], "test-0");
        assert_eq!(loaded.id_map[4], "test-4");

        // Cleanup
        let _ = std::fs::remove_file(&index_path);
        let _ = std::fs::remove_file(&id_map_path);
    }

    /// A `.tv` and an id map are renamed into place separately, so a crash (or a
    /// hand-restored backup) can pair a stale index with a fresh map of a
    /// different length. Every slot would then resolve to the wrong memory, so
    /// load must refuse the pair rather than trust the id-map length.
    #[test]
    fn test_load_rejects_a_pair_whose_lengths_disagree() {
        let dims = 32;
        let dir = std::env::temp_dir().join(format!("memex8-mixed-pair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let index_path = dir.join("pair.tv").to_string_lossy().to_string();
        let id_map_path = dir.join("pair-ids.json").to_string_lossy().to_string();

        {
            let mut index = TurboQuantVectorIndex::new(dims, 4);
            let ids: Vec<String> = (0..3).map(|i| format!("slot-{}", i)).collect();
            let vectors: Vec<Vec<f32>> = (0..3).map(|i| make_vector(dims, i)).collect();
            index.add_batch(&ids, &vectors);
            index.save(&index_path, &id_map_path).unwrap();
        }
        assert_eq!(
            TurboQuantVectorIndex::load(&index_path, &id_map_path, dims, 4)
                .unwrap()
                .slots(),
            3
        );

        // Keep the foreign 3-vector index, shrink the map to 2 IDs.
        std::fs::write(&id_map_path, r#"["slot-0","slot-1"]"#).unwrap();
        let error = match TurboQuantVectorIndex::load(&index_path, &id_map_path, dims, 4) {
            Ok(_) => panic!("a 3-vector index beside a 2-ID map must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(error.contains("holds 3 vectors"), "{error}");
        assert!(error.contains("lists 2 IDs"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_decide_bit_width_mapping() {
        // Unquantized (full precision): access >= 50 OR importance >= 0.95
        assert_eq!(decide_bit_width(50, 0.5), None);
        assert_eq!(decide_bit_width(10, 0.95), None);

        // 4-bit tier
        assert_eq!(decide_bit_width(20, 0.5), Some(4));
        assert_eq!(decide_bit_width(5, 0.8), Some(4));

        // 2-bit tier: access == 0 AND importance < 0.3
        assert_eq!(decide_bit_width(0, 0.1), Some(2));
        assert_eq!(decide_bit_width(0, 0.25), Some(2));

        // 2.5-bit legacy → maps to 4
        assert_eq!(decide_bit_width(3, 0.4), Some(4));

        // 3.5-bit legacy → maps to 4
        assert_eq!(decide_bit_width(10, 0.6), Some(4));
    }

    #[test]
    fn test_compression_ratio() {
        let idx = TurboQuantVectorIndex::new(768, 4);
        assert!((idx.compression_ratio() - 8.0).abs() < 0.01);

        let idx2 = TurboQuantVectorIndex::new(768, 2);
        assert!((idx2.compression_ratio() - 16.0).abs() < 0.01);
    }

    #[test]
    fn test_empty_search() {
        let index = TurboQuantVectorIndex::new(128, 4);
        let (scores, indices) = index.search(&[0.0; 128], 10);
        assert!(scores.is_empty());
        assert!(indices.is_empty());
    }
}
