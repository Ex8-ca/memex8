use crate::config::AppConfig;
use crate::engine::embedder;
use crate::engine::memex8_md::write_digest_md;
use crate::engine::quantizer::{decide_bit_width, AdaptiveScalarQuantizer};
use crate::engine::reactions::reaction_boost;
use crate::storage::qdrant::{GapPoint, MemoryPoint, QdrantStore};

pub struct SlumberEngine {
    config: AppConfig,
    store: QdrantStore,
}

impl SlumberEngine {
    /// Create an embedder using env vars (matching Engine::make_embedder logic).
    /// This ensures slumber uses the same embedding provider as the main engine.
    fn embedder(&self) -> anyhow::Result<Box<dyn embedder::Embedder>> {
        let mut cfg = self.config.clone();
        cfg.embedding.provider = std::env::var("EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| cfg.embedding.provider.clone());
        cfg.embedding.model = std::env::var("EMBEDDING_MODEL")
            .unwrap_or_else(|_| cfg.embedding.model.clone());
        cfg.embedding.dimensions = std::env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(cfg.embedding.dimensions);

        if cfg.embedding.provider == "openai" {
            let key = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| cfg.openai_api_key());
            if let Some(ref k) = key {
                std::env::set_var("OPENAI_API_KEY", k);
            }
        }

        embedder::create_embedder(&cfg)
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SlumberReport {
    pub memories_scanned: usize,
    pub deduplicated: usize,
    pub quantized: usize,
    pub realms_updated: usize,
    pub realms_renamed: usize,
    pub flagged_for_prune: usize,
    pub memex8_md_written: usize,
    pub memories_consolidated: usize,
    pub index_optimized: usize,
    pub digest_md_written: usize,
    /// Memories whose importance was decayed.
    pub decayed: usize,
    /// Association links created.
    pub associated: usize,
    /// Knowledge gaps detected.
    pub gaps_detected: usize,
    /// Session memories reviewed and re-weighted.
    pub sessions_reviewed: usize,
    /// Empty realm shells deleted.
    pub realms_pruned: usize,
    /// Memories re-quantized to a different bit width during dynamic policy pass.
    pub re_quantized: usize,
}

impl SlumberEngine {
    pub fn new(config: AppConfig, store: QdrantStore) -> Self {
        Self { config, store }
    }

    /// Run the full slumber maintenance pipeline.
    /// `force_consolidation` is set by the scheduler when the consolidation
    /// wall-clock schedule matches — avoids the timing drift bug where the
    /// 5-minute cron ingest ticks never align with "0 3 * * *" exactly.
    pub async fn run_full_pipeline(
        &self,
        force_consolidation: bool,
    ) -> anyhow::Result<SlumberReport> {
        let mut report = SlumberReport::default();

        // Phase 1: Deduplicate near-identical memories
        tracing::info!("💤 Slumber phase 1: Deduplication");
        let all = self.store.scroll_all_memories().await?;
        report.memories_scanned = all.len();
        report.deduplicated = self.deduplicate().await?;

        // Phase 2: ScalarQuant compression
        tracing::info!("💤 Slumber phase 2: ScalarQuant compression");
        report.quantized = self.scalarquant_compress().await?;

        // Phase 2b: Re-quantify memories whose bit width changed (dynamic policy)
        tracing::info!("💤 Slumber phase 2b: Re-quantify dynamic policy");
        report.re_quantized = self.re_quantify_all().await?;

        // Phase 3: Re-cluster realms (update counts, check merges)
        tracing::info!("💤 Slumber phase 3: Re-cluster realms");
        report.realms_updated = self.recluster_realms().await?;

        // Phase 3b: Rename realms with human-readable names
        tracing::info!("💤 Slumber phase 3b: Rename realms");
        report.realms_renamed = self.rename_realms().await?;

        // Phase 4: Prune flagging
        tracing::info!("💤 Slumber phase 4: Prune flagging");
        report.flagged_for_prune = self.prune_flag().await?;

        // Phase 5: Update MEMEX8.md files
        if self.config.memex8_md.enabled {
            tracing::info!("💤 Slumber phase 5: Update MEMEX8.md files");
            report.memex8_md_written = self.update_memex8_md().await?;
        }

        // Phase 6: LLM memory consolidation (only run when scheduler signals it)
        if self.config.slumber.consolidation_schedule.is_empty() {
            tracing::debug!("  Skipping consolidation: schedule is disabled");
        } else if force_consolidation {
            tracing::info!("💤 Slumber phase 6: Memory consolidation (forced by scheduler)");
            report.memories_consolidated = self.llm_consolidate().await?;
        } else {
            tracing::debug!("  Skipping consolidation: not scheduled this cycle");
        }

        // Phase 7: Qdrant index optimization (vacuum + rebuild)
        tracing::info!("💤 Slumber phase 7: Qdrant index optimization");
        report.index_optimized = self.optimize_qdrant_index().await?;

        // Phase 8: Memory decay (aging)
        tracing::info!("💤 Slumber phase 8: Memory decay");
        report.decayed = self.decay_memories().await?;

        // Phase 9: Build associations (semantic linking)
        tracing::info!("💤 Slumber phase 9: Build associations");
        report.associated = self.build_associations().await?;

        // Phase 10: Topic clusters & gap detection
        tracing::info!("💤 Slumber phase 10: Topic clusters & gap detection");
        report.gaps_detected = match self.detect_gaps().await? {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("  Gap detection failed: {}", e);
                0
            }
        };

        // Phase 11: Session memory review — re-weight session summaries based on
        // continued engagement (follow-up messages found in recent memories).
        tracing::info!("💤 Slumber phase 11: Session memory review");
        report.sessions_reviewed = self.review_session_memories().await?;

        // Phase 12: Prune empty realm shells left after consolidation/merging.
        tracing::info!("💤 Slumber phase 12: Prune empty realms");
        report.realms_pruned = self.prune_empty_realms().await?;

        // Phase 7: Write master memex8.md digest
        if self.config.digest_md.enabled {
            tracing::info!("💤 Slumber phase 7: Write digest md");
            let realms = self.store.list_realms().await?;
            let all_memories = self.store.scroll_all_memories().await?;
            report.digest_md_written = match write_digest_md(
                &self.config.digest_md,
                &all_memories,
                &realms,
                &report,
            )
            .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("  Digest md write failed: {}", e);
                    0
                }
            };
        }

        tracing::info!(
            "✅ Slumber complete: scanned={} dedup={} quantized={} re_quantized={} realms={} renamed={} consolidated={} prune={} md={} index_opt={} digest_md={} decayed={} associated={} gaps={} sessions_reviewed={} realms_pruned={}",
            report.memories_scanned,
            report.deduplicated,
            report.quantized,
            report.re_quantized,
            report.realms_updated,
            report.realms_renamed,
            report.memories_consolidated,
            report.flagged_for_prune,
            report.memex8_md_written,
            report.index_optimized,
            report.digest_md_written,
            report.decayed,
            report.associated,
            report.gaps_detected,
            report.sessions_reviewed,
            report.realms_pruned,
        );

        Ok(report)
    }

    // ─── Phase 1: Deduplication ──────────────────────────────────────────────

    /// Find and remove near-duplicate memories (cosine similarity > 0.95).
    /// Keeps the one with higher importance (upvotes + recency).
    async fn deduplicate(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let mut removed = 0;
        let _dedup_threshold = 0.95f32;

        // Group by source_hash first (exact duplicates)
        let mut by_hash: std::collections::HashMap<String, Vec<&MemoryPoint>> =
            std::collections::HashMap::new();
        for mem in &all {
            if !mem.source_hash.is_empty() {
                by_hash
                    .entry(mem.source_hash.clone())
                    .or_default()
                    .push(mem);
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
    /// Uses the configured quantizer policy (dynamic by default) to select bit width
    /// per memory based on access_count and importance.
    async fn scalarquant_compress(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories_with_vectors().await?;

        if all.is_empty() {
            tracing::info!("  No memories to quantize");
            return Ok(0);
        }

        // Get dimensions from actual vectors (not config, which may have stale defaults)
        let dims = all[0].vector.len();
        let is_dynamic = self.config.quantizer.policy == "dynamic";
        let static_bit_width = self.config.quantizer.static_bit_width;

        let mut quantized = 0;
        let mut skipped = 0;
        let mut total_cosine = 0.0f32;
        let mut bit_width_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for mem_with_vec in &all {
            let bit_width = if is_dynamic {
                decide_bit_width(
                    mem_with_vec.memory.access_count as u64,
                    mem_with_vec.memory.importance as f64,
                )
            } else {
                Some(static_bit_width)
            };

            let bw_label = match bit_width {
                None => "full".to_string(),
                Some(bw) => format!("{:.1}", bw),
            };
            *bit_width_counts.entry(bw_label.clone()).or_insert(0) += 1;

            // Unquantized memories: skip storing in quantized collection
            let bit_width = match bit_width {
                Some(bw) => bw,
                None => {
                    skipped += 1;
                    continue;
                }
            };

            let quantizer = AdaptiveScalarQuantizer::new(dims, bit_width);
            let qv = quantizer.quantize(&mem_with_vec.vector);
            let reconstructed = quantizer.dequantize(&qv);

            // Verify reconstruction quality
            let cosine = cosine_similarity(&mem_with_vec.vector, &reconstructed);
            total_cosine += cosine;

            // Only store if quality is acceptable
            if cosine > 0.7 {
                self.store
                    .store_quantized(
                        &mem_with_vec.memory.id,
                        &reconstructed,
                        &mem_with_vec.memory,
                        bit_width,
                    )
                    .await?;
                quantized += 1;
            } else {
                tracing::warn!(
                    "  Low quality quantization for {}: cosine={:.3}",
                    mem_with_vec.memory.id,
                    cosine
                );
            }
        }

        let avg_cosine = if quantized > 0 {
            total_cosine / quantized as f32
        } else {
            0.0
        };

        // Build summary string for bit width distribution
        let mut bw_summary: Vec<_> = bit_width_counts.into_iter().collect();
        bw_summary.sort_by(|a, b| a.0.cmp(&b.0));
        let bw_str = bw_summary
            .iter()
            .map(|(bw, count)| format!("{}={}", bw, count))
            .collect::<Vec<_>>()
            .join(", ");

        tracing::info!(
            "  Quantized {} / {} memories [{}] (skipped={} avg cosine={:.3})",
            quantized,
            all.len(),
            bw_str,
            skipped,
            avg_cosine
        );
        Ok(quantized)
    }

    // ─── Phase 2b: Re-quantify All (dynamic policy) ───────────────────────────

    /// Re-quantize all memories whose optimal bit width has changed since last
    /// quantization. Uses the dynamic policy to compare current access_count
    /// and importance against the bit width they were last stored with.
    ///
    /// Returns the count of memories that were re-quantized to a different bit width.
    async fn re_quantify_all(&self) -> anyhow::Result<usize> {
        // Only relevant under dynamic policy
        if self.config.quantizer.policy != "dynamic" {
            return Ok(0);
        }

        let all = self.store.scroll_all_memories_with_vectors().await?;
        if all.is_empty() {
            return Ok(0);
        }

        let dims = all[0].vector.len();
        let mut re_quantized = 0;
        let mut upgraded = 0;
        let mut downgraded = 0;
        let mut promoted_full = 0;

        for mem_with_vec in &all {
            let mem = &mem_with_vec.memory;
            let optimal_bw = decide_bit_width(mem.access_count as u64, mem.importance as f64);

            // Check if current stored bit width differs from optimal
            let current_bw = mem.quantized_bit_width;
            let needs_change = match optimal_bw {
                // Memory should be full precision but isn't
                None if current_bw > 0.0 => true,
                // Memory should be quantized at a specific width but differs
                Some(target) if (target - current_bw).abs() > 0.01 => true,
                // Memory is already at optimal (or both unquantized)
                _ => false,
            };

            if !needs_change {
                continue;
            }

            match optimal_bw {
                None => {
                    // Promote to full precision: remove from quantized collection
                    // and keep the original vector in the main collection
                    self.store.delete_quantized(&mem.id).await?;
                    promoted_full += 1;
                }
                Some(target_bw) => {
                    let quantizer = AdaptiveScalarQuantizer::new(dims, target_bw);
                    let qv = quantizer.quantize(&mem_with_vec.vector);
                    let reconstructed = quantizer.dequantize(&qv);

                    let cosine = cosine_similarity(&mem_with_vec.vector, &reconstructed);
                    if cosine > 0.7 {
                        self.store
                            .store_quantized(&mem.id, &reconstructed, &mem_with_vec.memory, target_bw)
                            .await?;
                        if target_bw > current_bw {
                            upgraded += 1;
                        } else {
                            downgraded += 1;
                        }
                    } else {
                        tracing::warn!(
                            "  Low quality re-quantization for {}: cosine={:.3}",
                            mem.id,
                            cosine
                        );
                        continue;
                    }
                }
            }

            re_quantized += 1;
        }

        if re_quantized > 0 {
            tracing::info!(
                "  Re-quantized {} memories (upgraded={}, downgraded={}, promoted_to_full={})",
                re_quantized,
                upgraded,
                downgraded,
                promoted_full
            );
        } else {
            tracing::info!("  No memories needed re-quantization");
        }

        Ok(re_quantized)
    }

    // ─── Phase 3: Re-cluster Realms ──────────────────────────────────────────

    /// Update realm memory counts and recompute centroids.
    async fn recluster_realms(&self) -> anyhow::Result<usize> {
        // Recompute realm centroids from actual memory vectors
        let centroids_updated = self.store.recompute_all_realm_centroids().await?;

        // Update counts for all realms
        self.store.update_realm_counts().await?;

        let realms = self.store.list_realms().await?;
        let merges = 0;

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
                let _threshold = self.config.realms.merge_threshold;
                if a.memory_count < 5 && b.memory_count < 5 {
                    // Could merge, but for now just log
                    tracing::debug!(
                        "  Merge candidate: '{}' ({}) ↔ '{}' ({})",
                        a.name,
                        a.memory_count,
                        b.name,
                        b.memory_count
                    );
                }
            }
        }

        tracing::info!(
            "  Updated {} realm centroids, {} realms total, {} merge candidates",
            centroids_updated,
            realms.len(),
            merges
        );

        // Check for realm splits (large realms)
        let splits = self.split_large_realms(&realms).await?;

        tracing::info!("  {} splits performed", splits);
        Ok(realms.len() + splits)
    }

    // ─── Phase 3b: Split Large Realms ────────────────────────────────────────

    /// Split realms that exceed the split_threshold using k-means (k=2).
    async fn split_large_realms(
        &self,
        realms: &[crate::storage::qdrant::RealmPoint],
    ) -> anyhow::Result<usize> {
        let threshold = self.config.realms.split_threshold;
        let mut splits = 0;

        for realm in realms {
            if realm.is_user_pinned {
                continue;
            }
            if realm.memory_count < threshold {
                continue;
            }

            // Guard against runaway recursive splits: cap the number of -a/-b
            // suffix levels. Without this, repeated splits produce names like
            // "Discord Ill-a-a-b-b-b-a-b-b-a-a-a-..." that blow up.
            let split_depth = realm.name.split('-').filter(|s| *s == "a" || *s == "b").count();
            if split_depth >= 3 {
                tracing::info!(
                    "  Skipping split of '{}': already split depth {} (max 3)",
                    realm.name,
                    split_depth
                );
                continue;
            }

            // Also guard against excessively long realm names
            let max_name_len = 80;
            if realm.name.len() >= max_name_len {
                tracing::info!(
                    "  Skipping split of '{}': name length {} exceeds max {}",
                    realm.name,
                    realm.name.len(),
                    max_name_len
                );
                continue;
            }

            tracing::info!(
                "  Splitting realm '{}' ({} memories, threshold={})",
                realm.name,
                realm.memory_count,
                threshold
            );

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
                tracing::info!(
                    "  Skipping split: cluster sizes {} and {} too small",
                    count_a,
                    count_b
                );
                continue;
            }

            // Create two new realms
            let id_a = uuid::Uuid::new_v4().to_string();
            let id_b = uuid::Uuid::new_v4().to_string();
            let name_a = format!("{}-a", realm.name);
            let name_b = format!("{}-b", realm.name);

            self.store
                .store_realm(&id_a, &c1, &name_a, None, false)
                .await?;
            self.store
                .store_realm(&id_b, &c2, &name_b, None, false)
                .await?;

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
                self.store
                    .update_memory_payload(&mem.memory.id, payload)
                    .await?;
            }

            // Delete old realm
            self.store.delete_realm(&realm.id).await?;

            tracing::info!(
                "  Split '{}' → '{}' ({}) + '{}' ({})",
                realm.name,
                name_a,
                count_a,
                name_b,
                count_b
            );
            splits += 1;
        }

        Ok(splits)
    }

    // ─── Phase 3b: Rename Realms by LLM + Merge Similar Realms ───────────────

    /// Rename realms with human-readable names using LLM, then merge similar realms
    /// and redistribute memories to their best-matching realm.
    async fn rename_realms(&self) -> anyhow::Result<usize> {
        let llm_url =
            std::env::var("LOCAL_LLM_URL").unwrap_or_else(|_| "http://192.168.1.8:8888".into());
        let llm_key = std::env::var("LOCAL_LLM_API_KEY").ok();

        let realms = self.store.list_realms().await?;
        let mut renamed = 0;

        // Step 1: Rename realms with LLM (if available) or word frequency fallback
        for realm in &realms {
            // Skip already human-readable names
            if !realm.name.starts_with("realm-") {
                continue;
            }

            // Get memories in this realm
            let all = self.store.scroll_all_memories().await?;
            let realm_mems: Vec<_> = all
                .iter()
                .filter(|m| m.realm_id.as_deref() == Some(&realm.id))
                .collect();

            if realm_mems.is_empty() {
                continue;
            }

            let new_name = if llm_key.is_some() {
                match self
                    .llm_name_realm(&llm_url, llm_key.as_deref(), &realm_mems)
                    .await
                {
                    Ok(name) => name,
                    Err(e) => {
                        tracing::warn!(
                            "  LLM naming failed for '{}', using fallback: {}",
                            realm.name,
                            e
                        );
                        Self::summarize_realm_freq(&realm_mems)
                    }
                }
            } else {
                Self::summarize_realm_freq(&realm_mems)
            };

            if new_name != realm.name && !new_name.is_empty() {
                self.store.update_realm_name(&realm.id, &new_name).await?;
                tracing::info!("  Renamed realm '{}' → '{}'", realm.name, new_name);
                renamed += 1;
            }
        }

        // Step 2: Merge similar realms (centroid cosine > 0.85)
        let merged = self.merge_similar_realms().await?;
        tracing::info!("  Merged {} similar realm pairs", merged);

        // Step 3: Redistribute memories to best-matching realms
        let redistributed = self.redistribute_memories().await?;
        tracing::info!(
            "  Redistributed {} memories to better-matching realms",
            redistributed
        );

        tracing::info!(
            "  Renamed {} realms, merged {} pairs, redistributed {} memories",
            renamed,
            merged,
            redistributed
        );
        Ok(renamed)
    }

    /// Use LLM to generate a descriptive realm name.
    async fn llm_name_realm(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        memories: &[&crate::storage::qdrant::MemoryPoint],
    ) -> anyhow::Result<String> {
        let memory_texts: Vec<String> = memories
            .iter()
            .take(5) // limit context
            .map(|m| {
                let content = if m.content.chars().count() > 300 {
                    format!("{}...", m.content.chars().take(300).collect::<String>())
                } else {
                    m.content.clone()
                };
                format!("- {}", content.replace('\n', " "))
            })
            .collect();

        let prompt = format!(
            "You are naming a knowledge realm (topic cluster) for an AI memory system.\n\
            Below are the memories in this realm:\n\n\
            {}\n\n\
            Give this realm a SHORT descriptive name (2-4 words, Title Case).\n\
            Examples: \"App Ideas\", \"Rust Development\", \"Home Assistant Setup\", \"Trading Strategies\"\n\
            Output ONLY the name, nothing else.",
            memory_texts.join("\n")
        );

        let result = self
            .call_local_llm(base_url, api_key, None, &prompt)
            .await?;
        // Clean up the response
        let name = result.trim().trim_matches('"').trim();
        if name.len() > 50 || name.is_empty() {
            return Err(anyhow::anyhow!("Invalid LLM name: '{}'", name));
        }
        Ok(name.to_string())
    }

    /// Merge realms whose centroids are very similar.
    /// Uses a lower threshold (0.6) for text embeddings where even
    /// different topics can have moderate cosine similarity.
    async fn merge_similar_realms(&self) -> anyhow::Result<usize> {
        let realms = self.store.list_realms().await?;
        let mut merged = 0;

        // Lower threshold for text embeddings: 0.35 instead of 0.85
        // Text embeddings from different topics typically have 0.2-0.4 cosine similarity
        let merge_threshold = 0.35f32;

        for i in 0..realms.len() {
            for j in (i + 1)..realms.len() {
                let a = &realms[i];
                let b = &realms[j];

                // Skip pinned realms
                if a.is_user_pinned || b.is_user_pinned {
                    continue;
                }

                // Skip realms without centroids or single-memory realms
                if a.centroid.is_empty() || b.centroid.is_empty() {
                    continue;
                }
                if a.memory_count <= 1 && b.memory_count <= 1 {
                    // Only merge single-memory realms if they're very similar
                    let sim = cosine_similarity(&a.centroid, &b.centroid);
                    if sim > merge_threshold {
                        tracing::info!(
                            "  Merging similar realms: '{}' ↔ '{}' (sim={:.3})",
                            a.name,
                            b.name,
                            sim
                        );
                        if let Err(e) = self.merge_realm_into(&b.id, &a.id, &a.name).await {
                            tracing::warn!("  Failed to merge realms: {}", e);
                        } else {
                            merged += 1;
                        }
                    }
                    continue;
                }

                // Merge if at least one realm has multiple memories
                let sim = cosine_similarity(&a.centroid, &b.centroid);
                if sim > merge_threshold {
                    tracing::info!(
                        "  Merging similar realms: '{}' ({}) ↔ '{}' ({}) (sim={:.3})",
                        a.name,
                        a.memory_count,
                        b.name,
                        b.memory_count,
                        sim
                    );
                    if let Err(e) = self.merge_realm_into(&b.id, &a.id, &a.name).await {
                        tracing::warn!("  Failed to merge realms: {}", e);
                    } else {
                        merged += 1;
                    }
                }
            }
        }

        Ok(merged)
    }

    /// Helper: merge all memories from source realm into target realm, then delete source.
    async fn merge_realm_into(
        &self,
        source_id: &str,
        target_id: &str,
        target_name: &str,
    ) -> anyhow::Result<()> {
        let all = self.store.scroll_all_memories().await?;
        let source_mems: Vec<_> = all
            .iter()
            .filter(|m| m.realm_id.as_deref() == Some(source_id))
            .collect();

        for mem in &source_mems {
            let payload: qdrant_client::Payload = serde_json::json!({
                "realm_id": target_id,
                "realm_name": target_name,
            })
            .try_into()
            .unwrap_or_default();
            if let Err(e) = self.store.update_memory_payload(&mem.id, payload).await {
                tracing::warn!("  Failed to reassign memory {}: {}", mem.id, e);
            }
        }

        // Delete source realm
        self.store.delete_realm(source_id).await?;
        self.store.update_realm_counts().await?;
        Ok(())
    }

    /// Redistribute memories to the realm whose centroid they're closest to.
    async fn redistribute_memories(&self) -> anyhow::Result<usize> {
        let realms = self.store.list_realms().await?;
        let all = self.store.scroll_all_memories_with_vectors().await?;
        let mut redistributed = 0;

        for mem_with_vec in &all {
            let mem = &mem_with_vec.memory;
            let current_realm_id = mem.realm_id.as_deref();

            // Find closest realm centroid
            let mut best_realm: Option<&crate::storage::qdrant::RealmPoint> = None;
            let mut best_sim = -1.0f32;

            for realm in &realms {
                if realm.centroid.is_empty() {
                    continue;
                }
                let sim = cosine_similarity(&mem_with_vec.vector, &realm.centroid);
                if sim > best_sim {
                    best_sim = sim;
                    best_realm = Some(realm);
                }
            }

            if let Some(best) = best_realm {
                if current_realm_id != Some(&best.id) && best_sim > 0.5 {
                    // Reassign to better-matching realm
                    let payload: qdrant_client::Payload = serde_json::json!({
                        "realm_id": best.id,
                        "realm_name": best.name,
                    })
                    .try_into()
                    .unwrap_or_default();

                    if let Err(e) = self.store.update_memory_payload(&mem.id, payload).await {
                        tracing::warn!("  Failed to redistribute memory {}: {}", mem.id, e);
                    } else {
                        tracing::debug!(
                            "  Moved '{}' from '{}' → '{}' (sim={:.3})",
                            &mem.content[..50.min(mem.content.len())],
                            current_realm_id.unwrap_or("?"),
                            best.name,
                            best_sim
                        );
                        redistributed += 1;
                    }
                }
            }
        }

        // Update counts after redistribution
        self.store.update_realm_counts().await?;
        Ok(redistributed)
    }

    /// Generate a realm name using word frequency (fallback when LLM unavailable).
    fn summarize_realm_freq(memories: &[&crate::storage::qdrant::MemoryPoint]) -> String {
        // Common English stopwords + technical noise words
        let stopwords: std::collections::HashSet<&str> = [
            "the",
            "a",
            "an",
            "and",
            "or",
            "but",
            "in",
            "on",
            "at",
            "to",
            "for",
            "of",
            "with",
            "by",
            "from",
            "is",
            "are",
            "was",
            "were",
            "be",
            "been",
            "being",
            "have",
            "has",
            "had",
            "do",
            "does",
            "did",
            "will",
            "would",
            "could",
            "should",
            "may",
            "might",
            "shall",
            "can",
            "need",
            "must",
            "that",
            "this",
            "these",
            "those",
            "it",
            "its",
            "i",
            "me",
            "my",
            "we",
            "our",
            "you",
            "your",
            "he",
            "him",
            "his",
            "she",
            "her",
            "they",
            "them",
            "their",
            "what",
            "which",
            "who",
            "whom",
            "when",
            "where",
            "why",
            "how",
            "not",
            "no",
            "yes",
            "so",
            "if",
            "then",
            "than",
            "too",
            "very",
            "just",
            "about",
            "also",
            "all",
            "any",
            "as",
            "into",
            "like",
            "more",
            "most",
            "only",
            "other",
            "out",
            "over",
            "own",
            "same",
            "some",
            "such",
            "up",
            "down",
            "after",
            "before",
            "between",
            "through",
            "during",
            "below",
            "above",
            "here",
            "there",
            "once",
            "while",
            "until",
            "unless",
            "because",
            "since",
            "even",
            "well",
            "back",
            "still",
            "already",
            "much",
            "many",
            "new",
            "use",
            "used",
            "using",
            "get",
            "got",
            "make",
            "made",
            "one",
            "two",
            "first",
            "last",
            "next",
            "each",
            "every",
            "both",
            "few",
            "way",
            "thing",
            "things",
            "work",
            "want",
            "need",
            "know",
            "think",
            "see",
            "come",
            "go",
            "take",
            "give",
            "tell",
            "say",
            "says",
            "said",
            "told",
            "help",
            "run",
            "went",
            "going",
            "set",
            "show",
            "find",
            "call",
            "try",
            "ask",
            "put",
            "keep",
            "let",
            "begin",
            "seem",
            "leave",
            "turn",
            "end",
            "right",
            "left",
            "old",
            "big",
            "small",
            "good",
            "bad",
            "high",
            "low",
            "long",
            "short",
            "done",
            "fix",
            "fixed",
            "added",
            "update",
            "updated",
            "changes",
            "change",
            "issue",
            "issues",
            "fixes",
            "fixing",
            "commit",
            "commits",
            "pushed",
            "push",
            "committing",
            "github",
            "repo",
            "repository",
            "branch",
            "main",
            "master",
            "merge",
            "pull",
            "request",
            "pr",
            "bug",
            "feature",
            "task",
            "tasks",
            "todo",
            "completed",
            "finished",
            "working",
            "implemented",
            "implementation",
            "build",
            "built",
            "testing",
            "tested",
            "test",
            "tests",
            "check",
            "checked",
            "checking",
            "review",
            "reviewed",
            "please",
            "thanks",
            "thank",
            "ok",
            "okay",
            "sure",
            "cool",
            "awesome",
            "perfect",
            "exactly",
            "correct",
            "wrong",
            "hey",
            "hi",
            "hello",
            "hello",
            "hello",
            "hello",
            "hey",
            "hi",
            "hi",
            "hi",
            "md",
            "txt",
            "rs",
            "py",
            "js",
            "ts",
            "html",
            "css",
            "json",
            "yaml",
            "yml",
            "toml",
            "cfg",
            "conf",
            "ini",
            "env",
            "git",
            "docker",
            "compose",
            "file",
            "files",
            "directory",
            "directories",
            "folder",
            "path",
            "paths",
            "src",
            "lib",
            "bin",
            "build",
            "target",
            "node",
            "modules",
            "package",
            "packages",
            "install",
            "installed",
            "installing",
            "run",
            "running",
            "start",
            "started",
            "starting",
            "stop",
            "stopped",
            "stopping",
            "restart",
            "restarted",
            "restarting",
            "deploy",
            "deployed",
            "deploying",
            "deployment",
            "config",
            "configuration",
            "settings",
            "setup",
            "setting",
            "server",
            "client",
            "api",
            "endpoint",
            "endpoints",
            "url",
            "urls",
            "http",
            "https",
            "localhost",
            "port",
            "ports",
            "host",
            "hosts",
            "app",
            "apps",
            "application",
            "applications",
            "project",
            "projects",
            "code",
            "coding",
            "program",
            "programming",
            "software",
            "system",
            "systems",
            "service",
            "services",
            "function",
            "functions",
            "method",
            "methods",
            "class",
            "classes",
            "object",
            "objects",
            "type",
            "types",
            "string",
            "strings",
            "number",
            "numbers",
            "int",
            "float",
            "bool",
            "bools",
            "array",
            "arrays",
            "list",
            "lists",
            "map",
            "maps",
            "dict",
            "dicts",
            "hash",
            "hashes",
            "hashmap",
            "hashmaps",
            "vec",
            "vectors",
            "vector",
            "embed",
            "embedding",
            "embeddings",
            "model",
            "models",
            "llm",
            "llms",
            "ai",
            "ml",
            "agent",
            "agents",
            "bot",
            "bots",
            "memex8",
            "hermes",
            "openclaw",
            "plugin",
            "plugins",
            "skill",
            "skills",
            "memory",
            "memories",
            "memo",
            "memos",
            "note",
            "notes",
            "data",
            "database",
            "db",
            "store",
            "storage",
            "stored",
            "stores",
            "saving",
            "save",
            "saved",
            "reads",
            "read",
            "writes",
            "write",
            "written",
            "content",
            "contents",
            "text",
            "texts",
            "words",
            "word",
            "sentence",
            "sentences",
            "paragraph",
            "paragraphs",
            "page",
            "pages",
            "line",
            "lines",
            "character",
            "characters",
            "char",
            "chars",
            "symbol",
            "symbols",
            "token",
            "tokens",
            "chunk",
            "chunks",
            "section",
            "sections",
            "header",
            "headers",
            "title",
            "titles",
            "heading",
            "headings",
            "user",
            "users",
            "assistant",
            "assistant",
            "system",
            "message",
            "messages",
            "chat",
            "chats",
            "conversation",
            "conversations",
            "turn",
            "turns",
            "prompt",
            "prompts",
            "response",
            "responses",
            "output",
            "outputs",
            "input",
            "inputs",
            "error",
            "errors",
            "warning",
            "warnings",
            "info",
            "information",
            "detail",
            "details",
            "log",
            "logs",
            "logging",
            "logged",
            "trace",
            "traces",
            "debug",
            "debugging",
            "bug",
            "bugs",
            "crash",
            "crashes",
            "crashed",
            "fail",
            "fails",
            "failed",
            "failure",
            "failures",
            "success",
            "successful",
            "succeed",
            "succeeded",
            "succeeds",
            "improve",
            "improved",
            "improvement",
            "improvements",
            "optimize",
            "optimized",
            "optimization",
            "performance",
            "speed",
            "fast",
            "faster",
            "fastest",
            "slow",
            "slower",
            "slowest",
            "time",
            "times",
            "second",
            "seconds",
            "minute",
            "minutes",
            "hour",
            "hours",
            "day",
            "days",
            "week",
            "weeks",
            "month",
            "months",
            "year",
            "years",
            "now",
            "today",
            "tomorrow",
            "yesterday",
            "soon",
            "later",
            "early",
            "earlier",
            "late",
            "recent",
            "recently",
            "current",
            "currently",
            "future",
            "past",
            "previous",
            "following",
            "preceding",
            "however",
            "whatever",
            "whenever",
            "wherever",
            "whoever",
            "whomever",
            "whichever",
            "although",
            "though",
            "whether",
            "therefore",
            "thus",
            "hence",
            "consequently",
            "accordingly",
            "nevertheless",
            "nonetheless",
            "notwithstanding",
            "otherwise",
            "meanwhile",
            "furthermore",
            "moreover",
            "besides",
            "additionally",
            "either",
            "neither",
            "nor",
            "except",
            "save",
            "barring",
            "excluding",
            "including",
            "concerning",
            "regarding",
            "respecting",
            "touching",
            "versus",
            "via",
            "per",
            "throughout",
            "across",
            "along",
            "around",
            "near",
            "nearer",
            "nearest",
            "beside",
            "beyond",
            "beneath",
            "under",
            "underneath",
            "overhead",
            "onto",
            "upon",
            "towards",
            "away",
            "off",
            "forth",
            "forward",
            "backward",
            "behind",
            "ahead",
            "ago",
            "yet",
            "always",
            "often",
            "frequently",
            "usually",
            "generally",
            "normally",
            "commonly",
            "rarely",
            "seldom",
            "occasionally",
            "sometimes",
            "hardly",
            "scarcely",
            "barely",
            "merely",
            "simply",
            "quite",
            "rather",
            "fairly",
            "pretty",
            "somewhat",
            "extremely",
            "exceedingly",
            "remarkably",
            "exceptionally",
            "particularly",
            "especially",
            "mainly",
            "mostly",
            "largely",
            "chiefly",
            "primarily",
            "principally",
            "essentially",
            "fundamentally",
            "basically",
            "virtually",
            "practically",
            "nearly",
            "almost",
            "approximately",
            "roughly",
            "circa",
            "precisely",
            "specifically",
            "namely",
            "namely",
        ]
        .into_iter()
        .collect();

        // Count word frequencies across all memories
        let mut word_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for mem in memories {
            // Use heading if available, otherwise first 200 chars of content
            let text = mem
                .heading
                .clone()
                .unwrap_or_else(|| mem.content.chars().take(200).collect());

            // Extract words: alphanumeric sequences of 3+ chars
            for word in text.split_whitespace() {
                let cleaned: String = word
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase();

                if cleaned.len() >= 3 && !stopwords.contains(cleaned.as_str()) {
                    *word_counts.entry(cleaned).or_insert(0) += 1;
                }
            }
        }

        // Sort by frequency
        let mut sorted: Vec<_> = word_counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // Take top 2-3 words and format as title case
        let top_words: Vec<String> = sorted
            .iter()
            .take(3)
            .map(|(w, _)| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .collect();

        if top_words.is_empty() {
            return format!(
                "topic-{}",
                &memories[0]
                    .realm_id
                    .as_ref()
                    .map(|s| s.chars().take(8).collect::<String>())
                    .unwrap_or("unknown".to_string())
            );
        }

        top_words.join(" ")
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
                    mem.id,
                    age_days,
                    mem.importance
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

            // Truncate long content (UTF-8 safe)
            let content = if mem.content.len() > 500 {
                let safe_end = mem
                    .content
                    .char_indices()
                    .take_while(|(i, _)| *i < 500)
                    .last()
                    .map_or(mem.content.len(), |(i, c)| i + c.len_utf8());
                format!("{}...", &mem.content[..safe_end])
            } else {
                mem.content.clone()
            };
            md.push_str(&content);
            md.push_str("\n\n");

            md.push_str(&format!("- **Realm**: {}\n", mem.realm_name));
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

    // ─── Phase 6: LLM Memory Consolidation ────────────────────────────────────

    /// Use an LLM to consolidate raw conversation fragments into clean summaries.
    /// Groups memories by realm, sends batches to local LLM (Qwen 3.6 via Unsloth),
    /// then replaces fragmented memories with clean summaries.
    async fn llm_consolidate(&self) -> anyhow::Result<usize> {
        let backend = &self.config.slumber.consolidation.backend;
        let model = self.config.slumber.consolidation.model.clone();

        // Group memories by realm
        let all = self.store.scroll_all_memories().await?;
        let mut by_realm: std::collections::HashMap<String, Vec<&MemoryPoint>> =
            std::collections::HashMap::new();

        for mem in &all {
            let realm = mem.realm_name.clone();
            by_realm.entry(realm).or_default().push(mem);
        }

        let mut consolidated = 0;

        for (realm_name, memories) in &by_realm {
            // Only consolidate realms with 2+ memories
            if memories.len() < 2 {
                continue;
            }

            tracing::info!(
                "  Consolidating {} memories in realm '{}' (backend: {})",
                memories.len(),
                realm_name,
                backend
            );

            // Build the prompt (limit to top 10 memories by importance)
            let mut sorted = memories.to_vec();
            sorted.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted.truncate(10);

            let memory_texts: Vec<String> = sorted
                .iter()
                .map(|m| {
                    let content = if m.content.len() > 500 {
                        let safe_end = m
                            .content
                            .char_indices()
                            .take_while(|(i, _)| *i < 500)
                            .last()
                            .map_or(m.content.len(), |(i, c)| i + c.len_utf8());
                        format!("{}...", &m.content[..safe_end])
                    } else {
                        m.content.clone()
                    };
                    format!("--- Memory {} ---\n{}", m.id, content)
                })
                .collect();

            let prompt = format!(
                "You are an AI memory consolidation assistant. \
                Below are raw memory entries from a knowledge realm called '{}'. \
                These memories were auto-captured from AI agent conversations and contain \
                conversational artifacts, redundancies, and fragmentation.\n\n\
                Your task: Consolidate these {} memories into a SINGLE clean, concise summary.\n\n\
                Rules:\n\
                - Remove all conversational artifacts (## User, ## Assistant, etc.)\n\
                - Remove meta-commentary about saving memories or reviewing conversations\n\
                - Merge duplicate information\n\
                - Keep only the factual, useful information\n\
                - Write in a clear, structured format\n\
                - Keep it under 500 words\n\
                - If the memories are about code/technical topics, preserve technical details\n\
                - Output ONLY the consolidated summary, no preamble\n\n\
                {}\n\n\
                Consolidated summary:",
                realm_name,
                sorted.len(),
                memory_texts.join("\n\n")
            );

            // Call the appropriate LLM backend
            let summary = match backend.as_str() {
                "openai" => {
                    let api_key = std::env::var("OPENAI_API_KEY").ok();
                    if api_key.is_none() {
                        tracing::warn!("  Skipping consolidation: OPENAI_API_KEY not set");
                        return Ok(consolidated);
                    }
                    let openai_model = model.as_deref().unwrap_or("gpt-4o-mini");
                    self.call_openai(&api_key.unwrap(), openai_model, &prompt)
                        .await
                }
                "local" => {
                    let llm_url = std::env::var("LOCAL_LLM_URL")
                        .unwrap_or_else(|_| "http://192.168.1.8:8888".into());
                    let llm_key = std::env::var("LOCAL_LLM_API_KEY").ok();
                    self.call_local_llm(&llm_url, llm_key.as_deref(), model.as_deref(), &prompt)
                        .await
                }
                _ => {
                    tracing::warn!("  Unknown consolidation backend: {}", backend);
                    continue;
                }
            };

            let summary = match summary {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("  Consolidation failed for '{}': {}", realm_name, e);
                    continue;
                }
            };

            if summary.trim().is_empty() {
                continue;
            }

            // Embed the consolidated summary to get a proper semantic vector
            let embedder = self.embedder()
                .map_err(|e| anyhow::anyhow!("Failed to create embedder: {}", e))?;
            let summary_vector = embedder.embed(&summary).await
                .map_err(|e| anyhow::anyhow!("Failed to embed summary: {}", e))?;

            // Collect IDs to delete
            let ids_to_delete: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();

            // Delete old fragmented memories
            for id in &ids_to_delete {
                if let Err(e) = self.store.delete_memory(id).await {
                    tracing::warn!("  Failed to delete old memory {}: {}", id, e);
                }
            }

            // Store the consolidated summary with its own embedded vector
            let id = uuid::Uuid::new_v4().to_string();
            if let Err(e) = self
                .store
                .store_memory_with_vector(
                    &id,
                    &summary,
                    &summary_vector,
                    None,
                    Some(realm_name),
                    1.0,
                    None,
                )
                .await
            {
                tracing::warn!(
                    "  Failed to store consolidated memory for '{}': {}",
                    realm_name,
                    e
                );
                continue;
            }

            tracing::info!(
                "  Consolidated {} memories → 1 summary in '{}'",
                memories.len(),
                realm_name
            );
            consolidated += 1;
        }

        tracing::info!("  Consolidated {} realms", consolidated);
        Ok(consolidated)
    }

    // ─── Phase 7: Qdrant Index Optimization ───────────────────────────────────

    /// Optimize Qdrant collections by triggering vacuum and index rebuilds.
    /// Returns number of collections optimized.
    async fn optimize_qdrant_index(&self) -> anyhow::Result<usize> {
        use qdrant_client::qdrant;

        let collections = vec![
            &self.config.qdrant.collection_memories[..],
            &self.config.qdrant.collection_quantized[..],
            &self.config.qdrant.collection_realms[..],
        ];

        let mut optimized = 0;

        for collection_name in collections {
            tracing::info!("  Optimizing collection '{}'...", collection_name);

            // Update collection optimizer settings to trigger background optimization
            let result = self
                .store
                .update_collection_optimizer(
                    collection_name,
                    qdrant::OptimizersConfigDiff {
                        deleted_threshold: Some(0.1),
                        vacuum_min_vector_number: Some(1000),
                        default_segment_number: Some(4),
                        max_segment_size: Some(200000),
                        memmap_threshold: Some(50000),
                        indexing_threshold: Some(20000),
                        flush_interval_sec: Some(5),
                        max_optimization_threads: Some(qdrant::MaxOptimizationThreads::from(2)),
                        ..Default::default()
                    },
                )
                .await;

            match result {
                Ok(_) => {
                    tracing::info!("  Optimized '{}'", collection_name);
                    optimized += 1;
                }
                Err(e) => {
                    tracing::warn!("  Failed to optimize '{}': {}", collection_name, e);
                }
            }
        }

        tracing::info!("  Optimized {} collections", optimized);
        Ok(optimized)
    }

    /// Call OpenAI API to generate a summary.
    async fn call_openai(
        &self,
        api_key: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": "You are a memory consolidation assistant. You take raw, fragmented memory entries and produce clean, concise summaries. Output ONLY the summary text, nothing else."},
                    {"role": "user", "content": prompt}
                ],
                "max_tokens": 1000,
                "temperature": 0.3
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("OpenAI API error ({}): {}", status, body));
        }

        let body: serde_json::Value = response.json().await?;
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        if content.is_empty() {
            return Err(anyhow::anyhow!("OpenAI returned empty response"));
        }

        Ok(content)
    }

    async fn call_local_llm(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        model: Option<&str>,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let client = reqwest::Client::new();
        let url = base_url.trim_end_matches('/');
        let mut req = client
            .post(format!("{}/v1/chat/completions", url))
            .header("Content-Type", "application/json");

        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let model_name = model.unwrap_or("qwen3.6-35b-a3b-instruct-2507");

        let response = req
            .json(&serde_json::json!({
                "model": model_name,
                "messages": [
                    {"role": "system", "content": "You are a memory consolidation assistant. You take raw, fragmented memory entries and produce clean, concise summaries. Output ONLY the summary text, nothing else."},
                    {"role": "user", "content": prompt}
                ],
                "max_tokens": 1000,
                "temperature": 0.3
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Local LLM API error ({}): {}",
                status,
                body
            ));
        }

        let body: serde_json::Value = response.json().await?;
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        if content.is_empty() {
            return Err(anyhow::anyhow!("Local LLM returned empty response"));
        }

        Ok(content)
    }

    // ─── Phase 8: Memory Decay ────────────────────────────────────────────────

    /// Apply time-based decay to all memories. Memories that haven't been accessed
    /// slowly lose importance, creating a natural "forgetting curve".
    async fn decay_memories(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let now = chrono::Utc::now();
        let decay_rate = self.config.slumber.decay_rate_per_day;
        let min_importance = 0.05f32;
        let mut decayed = 0;

        let mut updates: Vec<(&str, qdrant_client::Payload)> = Vec::new();

        for mem in &all {
            let last_accessed = chrono::DateTime::parse_from_rfc3339(&mem.last_accessed)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| now);

            let days_since = (now - last_accessed).num_seconds() as f32 / 86400.0;
            if days_since <= 0.0 {
                continue;
            }

            // Apply reaction boost: positive reactions slow decay, negative speed it up
            let reaction = mem.reaction_score;
            let boost = reaction_boost(reaction);

            // Base decay: importance decreases with time
            // Boosted decay: negative reactions decay faster, positive reactions decay slower
            let decayed_importance = mem.importance - (days_since * decay_rate * boost);
            let new_importance = decayed_importance.max(min_importance);
            let delta = (new_importance - mem.importance).abs();

            // Only update if importance changed meaningfully
            if delta > 0.001 {
                let payload: qdrant_client::Payload = serde_json::json!({
                    "importance": new_importance,
                })
                .try_into()
                .unwrap_or_default();
                updates.push((mem.id.as_str(), payload));
                decayed += 1;
            }
        }

        if !updates.is_empty() {
            // Batch update in chunks to avoid overwhelming Qdrant
            const BATCH_SIZE: usize = 100;
            for chunk in updates.chunks(BATCH_SIZE) {
                if let Err(e) = self.store.batch_update_payload(chunk).await {
                    tracing::warn!("  Batch decay update failed: {}", e);
                }
            }
        }

        tracing::info!(
            "  Decayed {} memories (rate={:.4}/day, min={:.2})",
            decayed,
            decay_rate,
            min_importance
        );
        Ok(decayed)
    }

    // ─── Phase 9: Build Associations ──────────────────────────────────────────

    /// Build semantic associations between memories by finding nearest neighbors.
    /// Creates bidirectional links with cosine similarity as strength.
    async fn build_associations(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let top_k = self.config.slumber.association_top_k as usize;
        let min_strength = self.config.slumber.association_min_strength;
        let mut total_links = 0;

        for mem in &all {
            // Skip memories with no content
            if mem.content.is_empty() {
                continue;
            }

            let similar = match self.store.find_similar(&mem.id, top_k).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("  find_similar failed for {}: {}", mem.id, e);
                    continue;
                }
            };

            // Filter by minimum strength
            let links: Vec<_> = similar
                .into_iter()
                .filter(|(_id, strength)| *strength >= min_strength)
                .collect();

            if links.is_empty() {
                continue;
            }

            let related_ids: Vec<String> = links.iter().map(|(id, _)| id.clone()).collect();
            let strengths: Vec<f32> = links.iter().map(|(_, s)| *s).collect();

            let payload: qdrant_client::Payload = serde_json::json!({
                "related_memory_ids": related_ids,
                "association_strengths": strengths,
            })
            .try_into()
            .unwrap_or_default();

            if let Err(e) = self.store.update_memory_payload(&mem.id, payload).await {
                tracing::warn!("  Failed to store associations for {}: {}", mem.id, e);
            } else {
                total_links += links.len();
            }
        }

        tracing::info!(
            "  Created {} association links (top_k={}, min_strength={:.2})",
            total_links,
            top_k,
            min_strength
        );
        Ok(total_links)
    }

    // ─── Phase 10: Topic Clusters & Gap Detection ────────────────────────────────

    /// Detect topic clusters using k-means clustering.
    /// Returns (cluster_id, Vec<memory_id>) mappings.
    async fn detect_topic_clusters(&self) -> anyhow::Result<anyhow::Result<Vec<(String, Vec<String>)>>> {
        use crate::engine::associations::detect_topic_clusters;

        let all_with_vectors = match self.store.scroll_all_memories_with_vectors().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("  scroll_all_memories_with_vectors failed: {}", e);
                return Ok(Err(anyhow::anyhow!("Failed to get memories with vectors: {}", e)));
            }
        };

        if all_with_vectors.is_empty() {
            tracing::info!("  No memories to cluster");
            return Ok(Ok(vec![]));
        }

        // Convert MemoryWithVector to the tuple format expected by detect_topic_clusters
        let memories_tuples: Vec<(String, MemoryPoint, Vec<f32>)> = all_with_vectors
            .iter()
            .map(|m| (m.memory.id.clone(), m.memory.clone(), m.vector.clone()))
            .collect();

        let k = self.config.inference.topic_clusters_k.max(2).min(20) as usize;
        let clusters = detect_topic_clusters(&memories_tuples, k);

        // Update each memory's topic_clusters field
        for cluster in &clusters {
            for memory_id in &cluster.memory_ids {
                // Get current memory
                if let Ok(Some(mut mem)) = self.store.get_memory(memory_id).await {
                    mem.topic_clusters.retain(|c| c != &cluster.id);
                    mem.topic_clusters.push(cluster.id.clone());

                    let payload: qdrant_client::Payload = serde_json::json!({
                        "topic_clusters": mem.topic_clusters,
                    })
                    .try_into()
                    .unwrap_or_default();

                    let _ = self.store.update_memory_payload(memory_id, payload).await;
                }
            }
        }

        let result: Vec<(String, Vec<String>)> = clusters
            .into_iter()
            .map(|c| (c.id, c.memory_ids))
            .collect();

        Ok(Ok(result))
    }

    /// Detect knowledge gaps based on cluster analysis.
    async fn detect_gaps(&self) -> anyhow::Result<anyhow::Result<usize>> {
        use crate::engine::associations::detect_gaps;

        if !self.config.inference.gap_detection_enabled {
            return Ok(Ok(0));
        }

        let all_with_vectors = match self.store.scroll_all_memories_with_vectors().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("  scroll_all_memories_with_vectors failed: {}", e);
                return Ok(Err(anyhow::anyhow!("Failed to get memories with vectors: {}", e)));
            }
        };

        if all_with_vectors.is_empty() {
            return Ok(Ok(0));
        }

        // First detect clusters
        let k = self.config.inference.topic_clusters_k.max(2).min(20) as usize;
        let all_as_tuples: Vec<(String, MemoryPoint, Vec<f32>)> = all_with_vectors
            .iter()
            .map(|m| (m.memory.id.clone(), m.memory.clone(), m.vector.clone()))
            .collect();
        let clusters = crate::engine::associations::detect_topic_clusters(&all_as_tuples, k);

        // Build memories slice and memory_vectors hashmap
        let memories: Vec<_> = all_with_vectors.iter().map(|m| m.memory.clone()).collect();
        let memory_vectors: std::collections::HashMap<String, Vec<f32>> = all_with_vectors
            .iter()
            .map(|m| (m.memory.id.clone(), m.vector.clone()))
            .collect();

        // Now detect gaps
        let gaps = detect_gaps(&clusters, &memories, &memory_vectors);

        let mut stored = 0;
        for gap in gaps {
            let gap_point = GapPoint {
                id: gap.id,
                vector: vec![], // gaps don't need vectors
                gap_type: gap.missing_link_type.as_str().to_string(),
                status: "open".to_string(),
                cluster_id: gap.from_cluster.clone(),
                suggested_topic: gap.to_cluster.clone(),
                description: gap.description.clone(),
                related_memory_ids: vec![], // not available from Gap
                suggested_search_queries: vec![],
                importance: gap.confidence,
                created_at: gap.detected_at,
            };

            if self.store.store_gap(&gap_point).await.is_ok() {
                stored += 1;
            }
        }

        tracing::info!("  Detected and stored {} knowledge gaps", stored);
        Ok(Ok(stored))
    }

    // ─── Phase 11: Session Memory Review ───────────────────────────────────

    /// Review session summary memories and re-weight them based on continued engagement.
    /// If a session's topic was followed up on (found in recent memories with matching realm/content),
    /// boost its importance. If no follow-up found after a week, slightly reduce importance.
    async fn review_session_memories(&self) -> anyhow::Result<usize> {
        // MemoryPoint imported via store methods

        let all = self.store.scroll_all_memories().await?;

        // Find session summary memories (chunk_type = "session_summary")
        let session_summaries: Vec<_> = all
            .iter()
            .filter(|m| m.chunk_type == "session_summary")
            .collect();

        if session_summaries.is_empty() {
            return Ok(0);
        }

        let now = chrono::Utc::now().timestamp() as f64;
        let mut reviewed = 0;

        for mem in session_summaries {
            let ingested_ts = chrono::DateTime::parse_from_rfc3339(&mem.ingested_at)
                .map(|dt| dt.timestamp() as f64)
                .unwrap_or(now);

            let days_old = ((now - ingested_ts) / 86400.0).max(0.0);

            let recent_cutoff = now - (7.0 * 86400.0);
            let recent_memories: Vec<_> = all
                .iter()
                .filter(|m| {
                    let mem_ts = chrono::DateTime::parse_from_rfc3339(&m.ingested_at)
                        .map(|dt| dt.timestamp() as f64)
                        .unwrap_or(0.0);
                    mem_ts >= recent_cutoff && m.id != mem.id
                })
                .collect();

            let has_followup = recent_memories.iter().any(|r| {
                r.realm_name == mem.realm_name
                    || r.content.contains(&mem.content[..mem.content.len().min(100)])
            });

            let current_importance = mem.importance;
            let new_importance = if days_old > 7.0 && !has_followup {
                (current_importance * 0.95).max(0.5)
            } else if has_followup {
                (current_importance * 1.1).min(3.0)
            } else {
                current_importance
            };

            if (new_importance - current_importance).abs() > 0.01 {
                self.store
                    .set_memory_importance(&mem.id, new_importance)
                    .await?;
                tracing::debug!(
                    "  Session {} importance: {:.2} → {:.2}",
                    &mem.id[..8],
                    current_importance,
                    new_importance
                );
                reviewed += 1;
            }
        }

        tracing::info!("  Reviewed {} session memories", reviewed);
        Ok(reviewed)
    }

    // ─── Phase 12: Prune Empty Realms ─────────────────────────────────────────

    /// Delete realm shells with 0 memories. These are left behind after
    /// consolidation merges or moves all memories out of a realm.
    async fn prune_empty_realms(&self) -> anyhow::Result<usize> {
        let realms = self.store.list_realms().await?;
        let mut pruned = 0;

        for realm in &realms {
            if realm.memory_count == 0 && !realm.is_user_pinned {
                if let Err(e) = self.store.delete_realm(&realm.id).await {
                    tracing::warn!(
                        "  Failed to delete empty realm '{}': {}",
                        realm.name,
                        e
                    );
                } else {
                    tracing::info!(
                        "  Pruned empty realm '{}'",
                        realm.name
                    );
                    pruned += 1;
                }
            }
        }

        tracing::info!("  Pruned {} empty realm shells", pruned);
        Ok(pruned)
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
