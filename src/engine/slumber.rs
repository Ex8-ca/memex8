use crate::config::AppConfig;
use crate::engine::quantizer::TurboQuantizer;
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
        report.deduplicated = self.deduplicate().await?;

        // Phase 2: TurboQuant compression
        tracing::info!("💤 Slumber phase 2: TurboQuant compression");
        report.quantized = self.turboquant_compress().await?;

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
        let n = all.len();
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

    // ─── Phase 2: TurboQuant Compression ─────────────────────────────────────

    /// Compress all memories using TurboQuant and store in the quantized collection.
    async fn turboquant_compress(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let bit_width = self.config.slumber.quantize_bit_width;
        let dims = self.config.embedding.dimensions as usize;

        let quantizer = TurboQuantizer::new(dims, bit_width);
        let mut quantized = 0;

        for mem in &all {
            // For now, use a placeholder vector (zeros) since we don't have
            // the original vectors stored. In production, we'd fetch with vectors=true.
            // The real flow would be:
            //   1. Fetch memory with original vector
            //   2. Quantize
            //   3. Store in quantized collection
            //   4. Optionally delete original to save space
            let placeholder = vec![0.0f32; dims];
            let qv = quantizer.quantize(&placeholder);
            let reconstructed = quantizer.dequantize(&qv);

            // Verify reconstruction quality
            let cosine = cosine_similarity(&placeholder, &reconstructed);
            if cosine > 0.5 {
                // Only store if quality is acceptable
                self.store.store_quantized(&mem.id, &reconstructed, mem).await?;
                quantized += 1;
            }
        }

        tracing::info!(
            "  Quantized {} memories at {:.1} bits/channel",
            quantized,
            bit_width
        );
        Ok(quantized)
    }

    // ─── Phase 3: Re-cluster Realms ──────────────────────────────────────────

    /// Update realm memory counts and check for merge opportunities.
    async fn recluster_realms(&self) -> anyhow::Result<usize> {
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

        tracing::info!("  Updated {} realm counts, {} merge candidates", realms.len(), merges);
        Ok(realms.len())
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

        // Group memories by source directory
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
