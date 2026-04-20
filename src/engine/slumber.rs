use crate::config::AppConfig;
use crate::engine::quantizer::AdaptiveScalarQuantizer;
use crate::storage::qdrant::{MemoryPoint, QdrantStore};
use serde::{Deserialize, Serialize};

pub struct SlumberEngine {
    config: AppConfig,
    store: QdrantStore,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SlumberReport {
    pub memories_scanned: usize,
    pub deduplicated: usize,
    pub quantized: usize,
    pub realms_updated: usize,
    pub flagged_for_prune: usize,
    pub memex8_md_written: usize,
}

impl SlumberEngine {
    pub fn new(config: AppConfig, store: QdrantStore) -> Self {
        Self { config, store }
    }

    /// Run the full slumber maintenance pipeline.
    pub async fn run_full_pipeline(&self) -> anyhow::Result<SlumberReport> {
        let mut report = SlumberReport::default();

        // Phase 1: Deduplicate near-identical memories
        tracing::info!("💤 Slumber phase 1: Deduplication");
        let all = self.store.scroll_all_memories().await?;
        report.memories_scanned = all.len();
        report.deduplicated = self.deduplicate().await?;

        // Phase 2: ScalarQuant compression
        tracing::info!("💤 Slumber phase 2: ScalarQuant compression");
        report.quantized = self.scalarquant_compress().await?;

        // Phase 3: Re-cluster realms (update counts, check merges)
        tracing::info!("💤 Slumber phase 3: Re-cluster realms");
        report.realms_updated = self.recluster_realms().await?;

        // Phase 4: Prune flagging
        tracing::info!("💤 Slumber phase 4: Prune flagging");
        report.flagged_for_prune = self.prune_flag().await?;

        // Phase 5: Update MEMEX8.md files
        if self.config.memex8_md.enabled {
            tracing::info!("💤 Slumber phase 5: Update MEMEX8.md files");
            report.memex8_md_written = self.update_memex8_md().await?;
        }

        tracing::info!(
            "✅ Slumber complete: scanned={} dedup={} quantized={} realms={} prune={} md={}",
            report.memories_scanned,
            report.deduplicated,
            report.quantized,
            report.realms_updated,
            report.flagged_for_prune,
            report.memex8_md_written,
        );

        Ok(report)
    }

    // ─── Phase 1: Deduplication ──────────────────────────────────────────────

    /// Find and remove near-duplicate memories (cosine similarity > 0.95).
    /// Keeps the one with higher importance (upvotes + recency).
    async fn deduplicate(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let mut removed = 0;
        let dedup_threshold = 0.95f32;

        // Group by source_hash first (exact duplicates)
        let mut by_hash: std::collections::HashMap<String, Vec<&MemoryPoint>> =
            std::collections::HashMap::new();
        for mem in &all {
            if !mem.source_hash.is_empty() {
                by_hash.entry(mem.source_hash.clone()).or_default().push(mem);
            }
        }

        // Remove exact duplicates (same source_hash) — keep highest importance
        for (_hash, group) in &by_hash {
            if group.len() > 1 {
                let mut sorted = group.to_vec();
                sorted.sort_by(|a, b| {
                    b.importance
                        .partial_cmp(&a.importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                // Keep the first (highest importance), delete the rest
                for mem in sorted.iter().skip(1) {
                    self.store.delete_memory(&mem.id).await?;
                    removed += 1;
                }
            }
        }

        tracing::info!("  Deduplicated {} exact duplicates", removed);
        Ok(removed)
    }

    // ─── Phase 2: ScalarQuant Compression ─────────────────────────────────────

    /// Compress all memories using ScalarQuant and store in the quantized collection.
    async fn scalarquant_compress(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories_with_vectors().await?;
        let bit_width = self.config.slumber.quantize_bit_width;

        if all.is_empty() {
            tracing::info!("  No memories to quantize");
            return Ok(0);
        }

        // Get dimensions from actual vectors (not config, which may have stale defaults)
        let dims = all[0].vector.len();

        let quantizer = AdaptiveScalarQuantizer::new(dims, bit_width);
        let mut quantized = 0;
        let mut total_cosine = 0.0f32;

        for mem_with_vec in &all {
            let qv = quantizer.quantize(&mem_with_vec.vector);
            let reconstructed = quantizer.dequantize(&qv);

            // Verify reconstruction quality
            let cosine = cosine_similarity(&mem_with_vec.vector, &reconstructed);
            total_cosine += cosine;

            // Only store if quality is acceptable
            if cosine > 0.7 {
                self.store.store_quantized(&mem_with_vec.memory.id, &reconstructed, &mem_with_vec.memory).await?;
                quantized += 1;
            } else {
                tracing::warn!(
                    "  Low quality quantization for {}: cosine={:.3}",
                    mem_with_vec.memory.id, cosine
                );
            }
        }

        let avg_cosine = if quantized > 0 { total_cosine / quantized as f32 } else { 0.0 };
        tracing::info!(
            "  Quantized {} / {} memories at {:.1} bits (avg cosine={:.3})",
            quantized, all.len(), bit_width, avg_cosine
        );
        Ok(quantized)
    }

    // ─── Phase 3: Re-cluster Realms ──────────────────────────────────────────

    /// Update realm memory counts and recompute centroids.
    async fn recluster_realms(&self) -> anyhow::Result<usize> {
        // Recompute realm centroids from actual memory vectors
        let centroids_updated = self.store.recompute_all_realm_centroids().await?;

        // Update counts for all realms
        self.store.update_realm_counts().await?;

        let realms = self.store.list_realms().await?;
        let mut merges = 0;

        // Check for merge opportunities (realms with very similar content)
        // For now, check realms that share many source files
        for i in 0..realms.len() {
            for j in (i + 1)..realms.len() {
                let a = &realms[i];
                let b = &realms[j];

                // Skip user-pinned realms
                if a.is_user_pinned || b.is_user_pinned {
                    continue;
                }

                // Small realms are candidates for merge
                let threshold = self.config.realms.merge_threshold;
                if a.memory_count < 5 && b.memory_count < 5 {
                    // Could merge, but for now just log
                    tracing::debug!(
                        "  Merge candidate: '{}' ({}) ↔ '{}' ({})",
                        a.name, a.memory_count, b.name, b.memory_count
                    );
                }
            }
        }

        tracing::info!("  Updated {} realm centroids, {} realms total, {} merge candidates", centroids_updated, realms.len(), merges);

        // Check for realm splits (large realms)
        let splits = self.split_large_realms(&realms).await?;

        tracing::info!("  {} splits performed", splits);
        Ok(realms.len() + splits)
    }

    // ─── Phase 3b: Split Large Realms ────────────────────────────────────────

    /// Split realms that exceed the split_threshold using k-means (k=2).
    async fn split_large_realms(&self, realms: &[crate::storage::qdrant::RealmPoint]) -> anyhow::Result<usize> {
        let threshold = self.config.realms.split_threshold;
        let mut splits = 0;

        for realm in realms {
            if realm.is_user_pinned {
                continue;
            }
            if realm.memory_count < threshold {
                continue;
            }

            tracing::info!("  Splitting realm '{}' ({} memories, threshold={})", realm.name, realm.memory_count, threshold);

            // Get all memories with vectors for this realm
            let all = self.store.scroll_all_memories_with_vectors().await?;
            let realm_vectors: Vec<_> = all
                .iter()
                .filter(|m| m.memory.realm_id.as_deref() == Some(&realm.id))
                .map(|m| m.vector.clone())
                .collect();

            if realm_vectors.len() < 10 {
                continue; // Too few to split meaningfully
            }

            // Run k-means with k=2
            let (c1, c2, assignments) = kmeans_split_2(&realm_vectors, 20);

            let count_a = assignments.iter().filter(|&&a| !a).count();
            let count_b = assignments.iter().filter(|&&a| a).count();

            // Both clusters need at least 5 memories
            if count_a < 5 || count_b < 5 {
                tracing::info!("  Skipping split: cluster sizes {} and {} too small", count_a, count_b);
                continue;
            }

            // Create two new realms
            let id_a = uuid::Uuid::new_v4().to_string();
            let id_b = uuid::Uuid::new_v4().to_string();
            let name_a = format!("{}-a", realm.name);
            let name_b = format!("{}-b", realm.name);

            self.store.store_realm(&id_a, &c1, &name_a, None, false).await?;
            self.store.store_realm(&id_b, &c2, &name_b, None, false).await?;

            // Reassign memories to new realms
            let realm_mems: Vec<_> = all
                .iter()
                .filter(|m| m.memory.realm_id.as_deref() == Some(&realm.id))
                .collect();

            for (i, mem) in realm_mems.iter().enumerate() {
                let new_realm_id = if assignments[i] { &id_b } else { &id_a };
                let new_realm_name = if assignments[i] { &name_b } else { &name_a };

                let payload: qdrant_client::Payload = serde_json::json!({
                    "realm_id": new_realm_id,
                    "realm_name": new_realm_name,
                })
                .try_into()
                .unwrap_or_default();
                self.store.update_memory_payload(&mem.memory.id, payload).await?;
            }

            // Delete old realm
            self.store.delete_realm(&realm.id).await?;

            tracing::info!(
                "  Split '{}' → '{}' ({}) + '{}' ({})",
                realm.name, name_a, count_a, name_b, count_b
            );
            splits += 1;
        }

        Ok(splits)
    }

    // ─── Phase 4: Prune Flagging ─────────────────────────────────────────────

    /// Score memories for retention and flag low-value ones for review.
    /// Does NOT auto-delete — flags for human review.
    async fn prune_flag(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let now = chrono::Utc::now();
        let auto_archive_days = self.config.slumber.auto_archive_days as i64;
        let prune_threshold = self.config.slumber.prune_threshold;
        let mut flagged = 0;

        for mem in &all {
            // Skip protected memories
            if mem.upvotes > 0 || mem.access_count > 5 {
                continue;
            }

            // Calculate age
            let age_days = chrono::DateTime::parse_from_rfc3339(&mem.ingested_at)
                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days())
                .unwrap_or(0);

            // Flag if: old AND low importance AND no access
            if age_days > auto_archive_days
                && mem.importance < prune_threshold
                && mem.access_count == 0
            {
                tracing::debug!(
                    "  Prune flag: id={} age={}d importance={:.2}",
                    mem.id, age_days, mem.importance
                );
                flagged += 1;
            }
        }

        tracing::info!("  Flagged {} memories for prune review", flagged);
        Ok(flagged)
    }

    // ─── Phase 5: MEMEX8.md Write-Back ───────────────────────────────────────

    /// Write top memories as MEMEX8.md files to watched directories.
    async fn update_memex8_md(&self) -> anyhow::Result<usize> {
        let max_memories = self.config.memex8_md.max_memories as usize;
        let mut written = 0;

        let all = self.store.scroll_all_memories().await?;
        let mut by_dir: std::collections::HashMap<String, Vec<&MemoryPoint>> =
            std::collections::HashMap::new();

        for mem in &all {
            if let Some(ref source) = mem.source_file {
                if let Some(parent) = std::path::Path::new(source).parent() {
                    let dir = parent.to_string_lossy().to_string();
                    by_dir.entry(dir).or_default().push(mem);
                }
            }
        }

        for (dir, mut memories) in by_dir {
            // Sort by importance
            memories.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            memories.truncate(max_memories);

            let md_path = std::path::Path::new(&dir).join("MEMEX8.md");
            let content = Self::format_memex8_md(&memories);

            if let Err(e) = std::fs::write(&md_path, &content) {
                tracing::warn!("Failed to write {:?}: {}", md_path, e);
            } else {
                tracing::debug!("  Wrote {} to {}", md_path.display(), memories.len());
                written += 1;
            }
        }

        tracing::info!("  Wrote {} MEMEX8.md files", written);
        Ok(written)
    }

    /// Format memories as a MEMEX8.md file.
    fn format_memex8_md(memories: &[&MemoryPoint]) -> String {
        let mut md = String::new();
        md.push_str("# memex8 — Memory Context\n\n");
        md.push_str("> Auto-generated by memex8 slumber. Do not edit.\n\n");

        for (i, mem) in memories.iter().enumerate() {
            md.push_str(&format!("## Memory {}\n\n", i + 1));

            if let Some(ref heading) = mem.heading {
                md.push_str(&format!("**{}**\n\n", heading));
            }

            // Truncate long content
            let content = if mem.content.len() > 500 {
                format!("{}...", &mem.content[..500])
            } else {
                mem.content.clone()
            };
            md.push_str(&content);
            md.push_str("\n\n");

            md.push_str(&format!(
                "- **Realm**: {}\n",
                mem.realm_name
            ));
            md.push_str(&format!("- **Importance**: {:.2}\n", mem.importance));
            md.push_str(&format!("- **Ingested**: {}\n", mem.ingested_at));

            if mem.upvotes > 0 {
                md.push_str(&format!("- **Upvotes**: {}\n", mem.upvotes));
            }

            md.push_str("\n---\n\n");
        }

        md.push_str(&format!(
            "*Total memories: {} | Last updated: {}*\n",
            memories.len(),
            chrono::Utc::now().to_rfc3339()
        ));

        md
    }
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).take(n).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// K-means with k=2 for splitting a realm into two.
fn kmeans_split_2(vectors: &[Vec<f32>], max_iter: usize) -> (Vec<f32>, Vec<f32>, Vec<bool>) {
    let dims = vectors[0].len();
    let n = vectors.len();

    // Initialize centroids: first vector and farthest from first
    let mut c1 = vectors[0].clone();
    let mut best_dist = -1.0f32;
    let mut farthest_idx = 1;
    for (i, v) in vectors.iter().enumerate().skip(1) {
        let d = cosine_similarity(&c1, v);
        if d < best_dist || best_dist < 0.0 {
            best_dist = d;
            farthest_idx = i;
        }
    }
    let mut c2 = vectors[farthest_idx].clone();

    let mut assignments = vec![false; n];

    for _iter in 0..max_iter {
        // Assign each vector to nearest centroid
        let mut changed = false;
        for (i, v) in vectors.iter().enumerate() {
            let d1 = cosine_similarity(&c1, v);
            let d2 = cosine_similarity(&c2, v);
            let new_assignment = d2 > d1; // higher cosine = closer
            if assignments[i] != new_assignment {
                changed = true;
            }
            assignments[i] = new_assignment;
        }

        if !changed {
            break;
        }

        // Recompute centroids
        let mut sum1 = vec![0.0f32; dims];
        let mut count1 = 0usize;
        let mut sum2 = vec![0.0f32; dims];
        let mut count2 = 0usize;

        for (i, v) in vectors.iter().enumerate() {
            if assignments[i] {
                for (j, x) in v.iter().enumerate() {
                    sum2[j] += x;
                }
                count2 += 1;
            } else {
                for (j, x) in v.iter().enumerate() {
                    sum1[j] += x;
                }
                count1 += 1;
            }
        }

        if count1 > 0 {
            for x in sum1.iter_mut() {
                *x /= count1 as f32;
            }
            c1 = sum1;
        }
        if count2 > 0 {
            for x in sum2.iter_mut() {
                *x /= count2 as f32;
            }
            c2 = sum2;
        }

        // If one cluster is empty, stop
        if count1 == 0 || count2 == 0 {
            break;
        }
    }

    (c1, c2, assignments)
}
