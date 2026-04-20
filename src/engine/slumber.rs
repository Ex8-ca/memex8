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
    pub realms_renamed: usize,
    pub flagged_for_prune: usize,
    pub memex8_md_written: usize,
    pub memories_consolidated: usize,
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

        // Phase 6: LLM memory consolidation
        tracing::info!("💤 Slumber phase 6: LLM memory consolidation");
        report.memories_consolidated = self.llm_consolidate().await?;

        tracing::info!(
            "✅ Slumber complete: scanned={} dedup={} quantized={} realms={} renamed={} consolidated={} prune={} md={}",
            report.memories_scanned,
            report.deduplicated,
            report.quantized,
            report.realms_updated,
            report.realms_renamed,
            report.memories_consolidated,
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

    // ─── Phase 3b: Rename Realms by LLM + Merge Similar Realms ───────────────

    /// Rename realms with human-readable names using LLM, then merge similar realms
    /// and redistribute memories to their best-matching realm.
    async fn rename_realms(&self) -> anyhow::Result<usize> {
        let openai_key = std::env::var("OPENAI_API_KEY").ok();

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
            let realm_mems: Vec<_> = all.iter()
                .filter(|m| m.realm_id.as_deref() == Some(&realm.id))
                .collect();

            if realm_mems.is_empty() {
                continue;
            }

            let new_name = if let Some(ref key) = openai_key {
                match self.llm_name_realm(key, &realm_mems).await {
                    Ok(name) => name,
                    Err(e) => {
                        tracing::warn!("  LLM naming failed for '{}', using fallback: {}", realm.name, e);
                        Self::summarize_realm_freq(&realm_mems)
                    }
                }
            } else {
                Self::summarize_realm_freq(&realm_mems)
            };

            if new_name != realm.name && !new_name.is_empty() {
                self.store.update_realm_name(&realm.id, &new_name).await?;
                tracing::info!(
                    "  Renamed realm '{}' → '{}'",
                    realm.name, new_name
                );
                renamed += 1;
            }
        }

        // Step 2: Merge similar realms (centroid cosine > 0.85)
        let merged = self.merge_similar_realms().await?;
        tracing::info!("  Merged {} similar realm pairs", merged);

        // Step 3: Redistribute memories to best-matching realms
        let redistributed = self.redistribute_memories().await?;
        tracing::info!("  Redistributed {} memories to better-matching realms", redistributed);

        tracing::info!("  Renamed {} realms, merged {} pairs, redistributed {} memories", renamed, merged, redistributed);
        Ok(renamed)
    }

    /// Use LLM to generate a descriptive realm name.
    async fn llm_name_realm(&self, api_key: &str, memories: &[&crate::storage::qdrant::MemoryPoint]) -> anyhow::Result<String> {
        let memory_texts: Vec<String> = memories.iter()
            .take(5) // limit context
            .map(|m| {
                let content = if m.content.len() > 300 {
                    format!("{}...", &m.content[..300])
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

        let result = self.call_openai(api_key, &prompt).await?;
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
                            a.name, b.name, sim
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
                        a.name, a.memory_count, b.name, b.memory_count, sim
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
    async fn merge_realm_into(&self, source_id: &str, target_id: &str, target_name: &str) -> anyhow::Result<()> {
        let all = self.store.scroll_all_memories().await?;
        let source_mems: Vec<_> = all.iter()
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
            "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for",
            "of", "with", "by", "from", "is", "are", "was", "were", "be", "been",
            "being", "have", "has", "had", "do", "does", "did", "will", "would",
            "could", "should", "may", "might", "shall", "can", "need", "must",
            "that", "this", "these", "those", "it", "its", "i", "me", "my", "we",
            "our", "you", "your", "he", "him", "his", "she", "her", "they", "them",
            "their", "what", "which", "who", "whom", "when", "where", "why", "how",
            "not", "no", "yes", "so", "if", "then", "than", "too", "very", "just",
            "about", "also", "all", "any", "as", "into", "like", "more", "most",
            "only", "other", "out", "over", "own", "same", "some", "such", "up",
            "down", "after", "before", "between", "through", "during", "below",
            "above", "here", "there", "once", "while", "until", "unless", "because",
            "since", "even", "well", "back", "still", "already", "much", "many",
            "new", "use", "used", "using", "get", "got", "make", "made", "one",
            "two", "first", "last", "next", "each", "every", "both", "few",
            "way", "thing", "things", "work", "want", "need", "know", "think",
            "see", "come", "go", "take", "give", "tell", "say", "says", "said",
            "told", "help", "run", "went", "going", "set", "show", "find", "call",
            "try", "ask", "put", "keep", "let", "begin", "seem", "leave", "turn",
            "end", "right", "left", "old", "big", "small", "good", "bad", "high",
            "low", "long", "short", "done", "fix", "fixed", "added", "update",
            "updated", "changes", "change", "issue", "issues", "fixes", "fixing",
            "commit", "commits", "pushed", "push", "committing", "github", "repo",
            "repository", "branch", "main", "master", "merge", "pull", "request",
            "pr", "bug", "feature", "task", "tasks", "todo", "completed", "finished",
            "working", "implemented", "implementation", "build", "built", "testing",
            "tested", "test", "tests", "check", "checked", "checking", "review",
            "reviewed", "please", "thanks", "thank", "ok", "okay", "sure", "cool",
            "awesome", "perfect", "exactly", "correct", "wrong", "hey", "hi",
            "hello", "hello", "hello", "hello", "hey", "hi", "hi", "hi",
            "md", "txt", "rs", "py", "js", "ts", "html", "css", "json", "yaml",
            "yml", "toml", "cfg", "conf", "ini", "env", "git", "docker", "compose",
            "file", "files", "directory", "directories", "folder", "path", "paths",
            "src", "lib", "bin", "build", "target", "node", "modules", "package",
            "packages", "install", "installed", "installing", "run", "running",
            "start", "started", "starting", "stop", "stopped", "stopping", "restart",
            "restarted", "restarting", "deploy", "deployed", "deploying", "deployment",
            "config", "configuration", "settings", "setup", "setting", "server",
            "client", "api", "endpoint", "endpoints", "url", "urls", "http", "https",
            "localhost", "port", "ports", "host", "hosts", "app", "apps", "application",
            "applications", "project", "projects", "code", "coding", "program",
            "programming", "software", "system", "systems", "service", "services",
            "function", "functions", "method", "methods", "class", "classes", "object",
            "objects", "type", "types", "string", "strings", "number", "numbers",
            "int", "float", "bool", "bools", "array", "arrays", "list", "lists",
            "map", "maps", "dict", "dicts", "hash", "hashes", "hashmap", "hashmaps",
            "vec", "vectors", "vector", "embed", "embedding", "embeddings", "model",
            "models", "llm", "llms", "ai", "ml", "agent", "agents", "bot", "bots",
            "memex8", "hermes", "openclaw", "plugin", "plugins", "skill", "skills",
            "memory", "memories", "memo", "memos", "note", "notes", "data", "database",
            "db", "store", "storage", "stored", "stores", "saving", "save", "saved",
            "reads", "read", "writes", "write", "written", "content", "contents",
            "text", "texts", "words", "word", "sentence", "sentences", "paragraph",
            "paragraphs", "page", "pages", "line", "lines", "character", "characters",
            "char", "chars", "symbol", "symbols", "token", "tokens", "chunk", "chunks",
            "section", "sections", "header", "headers", "title", "titles", "heading",
            "headings", "user", "users", "assistant", "assistant", "system", "message",
            "messages", "chat", "chats", "conversation", "conversations", "turn",
            "turns", "prompt", "prompts", "response", "responses", "output", "outputs",
            "input", "inputs", "error", "errors", "warning", "warnings", "info",
            "information", "detail", "details", "log", "logs", "logging", "logged",
            "trace", "traces", "debug", "debugging", "bug", "bugs", "crash", "crashes",
            "crashed", "fail", "fails", "failed", "failure", "failures", "success",
            "successful", "succeed", "succeeded", "succeeds", "improve", "improved",
            "improvement", "improvements", "optimize", "optimized", "optimization",
            "performance", "speed", "fast", "faster", "fastest", "slow", "slower",
            "slowest", "time", "times", "second", "seconds", "minute", "minutes",
            "hour", "hours", "day", "days", "week", "weeks", "month", "months",
            "year", "years", "now", "today", "tomorrow", "yesterday", "soon", "later",
            "early", "earlier", "late", "recent", "recently", "current", "currently",
            "future", "past", "previous", "following", "preceding", "however",
            "whatever", "whenever", "wherever", "whoever", "whomever", "whichever",
            "although", "though", "whether", "therefore", "thus", "hence",
            "consequently", "accordingly", "nevertheless", "nonetheless",
            "notwithstanding", "otherwise", "meanwhile", "furthermore", "moreover",
            "besides", "additionally", "either", "neither", "nor", "except", "save",
            "barring", "excluding", "including", "concerning", "regarding", "respecting",
            "touching", "versus", "via", "per", "throughout", "across", "along",
            "around", "near", "nearer", "nearest", "beside", "beyond", "beneath",
            "under", "underneath", "overhead", "onto", "upon", "towards", "away",
            "off", "forth", "forward", "backward", "behind", "ahead", "ago", "yet",
            "always", "often", "frequently", "usually", "generally", "normally",
            "commonly", "rarely", "seldom", "occasionally", "sometimes", "hardly",
            "scarcely", "barely", "merely", "simply", "quite", "rather", "fairly",
            "pretty", "somewhat", "extremely", "exceedingly", "remarkably",
            "exceptionally", "particularly", "especially", "mainly", "mostly",
            "largely", "chiefly", "primarily", "principally", "essentially",
            "fundamentally", "basically", "virtually", "practically", "nearly",
            "almost", "approximately", "roughly", "circa", "precisely", "specifically",
            "namely", "namely",
        ].into_iter().collect();

        // Count word frequencies across all memories
        let mut word_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for mem in memories {
            // Use heading if available, otherwise first 200 chars of content
            let text = mem.heading.clone().unwrap_or_else(|| {
                mem.content.chars().take(200).collect()
            });

            // Extract words: alphanumeric sequences of 3+ chars
            for word in text.split_whitespace() {
                let cleaned: String = word.chars()
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
        let top_words: Vec<String> = sorted.iter()
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
            return format!("topic-{}", &memories[0].realm_id.as_ref().map(|s| &s[..8]).unwrap_or("unknown"));
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

    // ─── Phase 6: LLM Memory Consolidation ────────────────────────────────────

    /// Use an LLM to consolidate raw conversation fragments into clean summaries.
    /// Groups memories by realm, sends batches to OpenAI for consolidation,
    /// then replaces fragmented memories with clean summaries.
    async fn llm_consolidate(&self) -> anyhow::Result<usize> {
        let openai_key = std::env::var("OPENAI_API_KEY").ok();
        if openai_key.is_none() {
            tracing::info!("  Skipping LLM consolidation: no OPENAI_API_KEY");
            return Ok(0);
        }
        let openai_key = openai_key.unwrap();

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
                "  Consolidating {} memories in realm '{}'",
                memories.len(),
                realm_name
            );

            // Build the prompt
            let memory_texts: Vec<String> = memories
                .iter()
                .map(|m| {
                    let content = if m.content.len() > 500 {
                        format!("{}...", &m.content[..500])
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
                memories.len(),
                memory_texts.join("\n\n")
            );

            // Call OpenAI
            let summary = match self.call_openai(&openai_key, &prompt).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("  OpenAI consolidation failed for '{}': {}", realm_name, e);
                    continue;
                }
            };

            if summary.trim().is_empty() {
                continue;
            }

            // Get centroid vector BEFORE deleting (for placement in vector space)
            let first_realm_id = memories.first().and_then(|m| m.realm_id.as_deref()).unwrap_or("").to_string();
            let first_vector = self
                .store
                .compute_realm_centroid(&first_realm_id)
                .await
                .ok()
                .flatten();

            // Collect IDs to delete
            let ids_to_delete: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();

            // Delete old fragmented memories
            for id in &ids_to_delete {
                if let Err(e) = self.store.delete_memory(id).await {
                    tracing::warn!("  Failed to delete old memory {}: {}", id, e);
                }
            }

            // Store the consolidated summary
            let id = uuid::Uuid::new_v4().to_string();
            if let Some(ref vector) = first_vector {
                if let Err(e) = self
                    .store
                    .store_memory_with_vector(
                        &id,
                        &summary,
                        vector,
                        None,
                        Some(realm_name),
                        1.0,
                        None,
                    )
                    .await
                {
                    tracing::warn!("  Failed to store consolidated memory for '{}': {}", realm_name, e);
                    continue;
                }
            } else {
                tracing::warn!("  No vector available for consolidated memory in '{}', skipping", realm_name);
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

    /// Call OpenAI API to generate a summary.
    async fn call_openai(&self, api_key: &str, prompt: &str) -> anyhow::Result<String> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": "gpt-4o-mini",
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
