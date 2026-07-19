pub mod associations;
pub mod backup;
pub mod chunker;
pub mod doctor;
pub mod embedder;
pub mod graph;
pub mod ingester;
pub mod memex8_md;
pub mod providers;
pub mod quantizer;
pub mod realms;
pub mod reactions;
pub mod scheduler;
pub mod session;
pub mod slumber;
pub mod watcher;

use crate::config::AppConfig;
pub use crate::engine::watcher::{FileChangeEvent, FileWatcher};
use crate::storage::qdrant::{MemoryWithVector, QdrantStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResult {
    pub id: String,
    pub content: String,
    pub heading: Option<String>,
    pub realm_name: String,
    pub importance: f32,
    pub score: f32,
    pub last_accessed: String,
    pub access_count: u32,
    pub upvotes: u32,
    /// Associated memory IDs from Phase 9 (semantic linking).
    #[serde(default)]
    pub related_memory_ids: Vec<String>,
    /// Cosine similarity strengths for each related memory (same order as related_memory_ids).
    #[serde(default)]
    pub association_strengths: Vec<f32>,
}

/// Soft score multiplier for recall-time verification down-weighting
/// (Phase A.2). Never filters — only re-weights so stale/contradicted
/// memories rank lower but still surface. See PLAN-memory-memex8.md.
pub(crate) fn verification_score_multiplier(status: &str) -> f32 {
    match status {
        "verified" => 1.0,
        "unverified" => 0.95, // slight penalty, not exclusion
        "stale" => 0.85,
        "contradicted" => 0.5,
        _ => 0.95,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlumberStatus {
    pub state: String, // "idle", "running", "paused"
    pub last_run: Option<String>,
    pub next_scheduled: Option<String>,
    pub memories_processed: u64,
    pub realms_reorganized: u32,
    pub last_report: Option<slumber::SlumberReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStats {
    pub total_memories: u64,
    pub total_realms: u32,
    pub storage_bytes: u64,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dimensions: u32,
    pub slumber_state: String,
}

pub struct Engine {
    config: AppConfig,
    store: QdrantStore,
    slumber_state: Arc<RwLock<SlumberState>>,
    last_activity: Arc<RwLock<tokio::time::Instant>>,
    /// Resolved embedding config (from env vars, overriding config.toml)
    embed_provider: String,
    embed_model: String,
    embed_dimensions: u32,
    openai_key: Option<String>,
    openai_base_url: String,
    /// File watcher (initialized on first use).
    file_watcher: Arc<RwLock<Option<FileWatcher>>>,
    /// Path to the config file (for persisting watch configs).
    config_path: String,
    /// TurboVec compressed vector index for fast search.
    /// Loaded from disk on startup, rebuilt during slumber.
    turbovec_index: Arc<RwLock<Option<quantizer::TurboQuantVectorIndex>>>,
}

struct SlumberState {
    status: String,
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    last_query: chrono::DateTime<chrono::Utc>,
    memories_processed: u64,
    realms_reorganized: u32,
    last_report: Option<slumber::SlumberReport>,
}

impl Engine {
    pub async fn new(config: AppConfig) -> anyhow::Result<Self> {
        // Override Qdrant URL from env var (set by docker-compose)
        let qdrant_url = std::env::var("QDRANT_URL").unwrap_or_else(|_| config.qdrant.url.clone());
        tracing::info!("Using Qdrant URL: {}", qdrant_url);
        let store = QdrantStore::new(&qdrant_url).await?;

        // Determine active embedding provider from env vars (set by docker-compose .env)
        let provider = std::env::var("EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| config.embedding.provider.clone());
        let model =
            std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| config.embedding.model.clone());
        let dimensions = std::env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(config.embedding.dimensions);

        // If using OpenAI but no key in config, try env var
        let openai_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .or_else(|| config.openai_api_key());
        let openai_base_url = std::env::var("OPENAI_BASE_URL")
            .ok()
            .unwrap_or_else(|| config.embedding.openai.base_url.clone());
        if provider == "openai" || provider == "openai-compatible" {
            if openai_key.is_none() {
                return Err(anyhow::anyhow!(
                    "OpenAI embedding provider selected but OPENAI_API_KEY is not set. \
                     Add OPENAI_API_KEY=sk-... to your .env file or set EMBEDDING_PROVIDER=ollama."
                ));
            }
            tracing::info!("Using OpenAI-compatible embeddings: {} at {} ({}d)", model, openai_base_url, dimensions);
        } else {
            tracing::info!("Using Ollama embeddings: {} ({}d)", model, dimensions);
        }

        store.ensure_collections(dimensions).await?;

        // Try to load TurboVec index from disk for fast search
        let turbovec_index = Self::init_turbovec_index(&config);

        Ok(Self {
            config,
            store,
            slumber_state: Arc::new(RwLock::new(SlumberState {
                status: "idle".into(),
                last_run: None,
                last_query: chrono::Utc::now(),
                memories_processed: 0,
                realms_reorganized: 0,
                last_report: None,
            })),
            last_activity: Arc::new(RwLock::new(tokio::time::Instant::now())),
            embed_provider: provider,
            embed_model: model,
            embed_dimensions: dimensions,
            openai_key,
            openai_base_url,
            file_watcher: Arc::new(RwLock::new(None)),
            config_path: "config.toml".to_string(),
            turbovec_index,
        })
    }

    /// Load TurboVec index from disk if it exists, otherwise return empty.
    fn init_turbovec_index(config: &AppConfig) -> Arc<RwLock<Option<quantizer::TurboQuantVectorIndex>>> {
        let index_path = config.turbovec.index_path.clone();
        let id_map_path = config.turbovec.id_map_path.clone();
        let bit_width = config.turbovec.bit_width;
        let dims = config.embedding.dimensions as usize;

        let loaded = quantizer::TurboQuantVectorIndex::load(&index_path, &id_map_path, dims, bit_width);
        match loaded {
            Ok(index) => {
                tracing::info!(
                    "📦 Loaded TurboVec index: {} vectors from {} ({}d @ {}-bit, {:.1}x compression)",
                    index.vector_count(),
                    index_path,
                    dims,
                    bit_width,
                    index.compression_ratio()
                );
                Arc::new(RwLock::new(Some(index)))
            }
            Err(e) => {
                tracing::debug!("TurboVec index not found at {} (will create on first slumber): {}", index_path, e);
                Arc::new(RwLock::new(None))
            }
        }
    }

    /// Handle to reset the idle activity timer (used by scheduler).
    pub fn activity_handle(&self) -> Arc<RwLock<tokio::time::Instant>> {
        self.last_activity.clone()
    }

    /// Reset the idle activity timer (call after each query).
    async fn touch_activity(&self) {
        *self.last_activity.write().await = tokio::time::Instant::now();
    }

    /// Create an embedder using the resolved config (from env vars).
    fn make_embedder(&self) -> anyhow::Result<Box<dyn embedder::Embedder>> {
        let mut cfg = self.config.clone();
        cfg.embedding.provider = self.embed_provider.clone();
        cfg.embedding.model = self.embed_model.clone();
        cfg.embedding.dimensions = self.embed_dimensions;
        if self.embed_provider == "openai" || self.embed_provider == "openai-compatible" {
            if let Some(ref key) = self.openai_key {
                std::env::set_var("OPENAI_API_KEY", key);
            }
            cfg.embedding.openai.base_url = self.openai_base_url.clone();
        }
        embedder::create_embedder(&cfg)
    }

    pub async fn ingest_path(
        &self,
        path: &str,
        chunk_by: &str,
        realm_hint: Option<&str>,
    ) -> anyhow::Result<()> {
        let ingester = ingester::Ingester::new(self.config.clone());
        let chunks = ingester.ingest_path(path, chunk_by).await?;
        tracing::info!("Ingested {} chunks from {}", chunks.len(), path);

        let embedder = self.make_embedder()?;
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let vectors = embedder.embed_batch(&texts).await?;

        for (i, chunk) in chunks.iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let vector = vectors.get(i).cloned().unwrap_or_default();

            // Assign to realm
            let realm_id = if let Some(hint) = realm_hint {
                self.store.find_realm_by_name(hint).await?.map(|r| r.id)
            } else {
                None
            };

            let realm_id = match realm_id {
                Some(rid) => rid,
                None => self.auto_assign_realm(&vector).await?,
            };

            let realm = self.store.get_realm(&realm_id).await?;
            let realm_name = realm.map(|r| r.name.clone()).unwrap_or_default();

            // Infer reaction score from content
            let reaction_score = crate::engine::reactions::infer_reaction(&chunk.content);

            self.store
                .store_memory(
                    &id,
                    &vector,
                    &chunk.content,
                    chunk.heading.as_deref(),
                    Some(chunk.source_file.as_str()),
                    &realm_id,
                    &realm_name,
                    &chunk.source_hash,
                    &chunk.chunk_type,
                    reaction_score,
                )
                .await?;

            {
                let mut state = self.slumber_state.write().await;
                state.memories_processed += 1;
            }
        }

        tracing::info!("Stored {} memories", chunks.len());
        Ok(())
    }

    async fn auto_assign_realm(&self, vector: &[f32]) -> anyhow::Result<String> {
        let realms = self.store.list_realms().await?;
        if realms.is_empty() {
            // Create a default realm
            let id = uuid::Uuid::new_v4().to_string();
            let name = "general".to_string();
            self.store
                .store_realm(&id, vector, &name, None, false)
                .await?;
            return Ok(id);
        }

        // Find closest realm by cosine similarity
        let mut best_realm = None;
        let mut best_score = -1.0f32;
        for realm in &realms {
            // Skip realms with no centroid yet
            if realm.centroid.is_empty() {
                continue;
            }
            let score = cosine_similarity(vector, &realm.centroid);
            if score > best_score {
                best_score = score;
                best_realm = Some(realm.clone());
            }
        }

        // Dynamic threshold: lower as we have more realms to avoid 1:1 ratio
        // For text embeddings, unrelated topics are ~0.2-0.4 cosine similarity
        let base_threshold = self.config.realms.similarity_threshold;
        let dynamic_threshold = if realms.len() > 20 {
            base_threshold * 0.5 // Lower threshold for many realms
        } else if realms.len() > 10 {
            base_threshold * 0.7
        } else {
            base_threshold
        };

        if let Some(realm) = best_realm {
            if best_score >= dynamic_threshold {
                return Ok(realm.id);
            }
        }

        // No close realm — create new one
        let id = uuid::Uuid::new_v4().to_string();
        let name = format!("realm-{}", &id[..8]);
        self.store
            .store_realm(&id, vector, &name, None, false)
            .await?;
        Ok(id)
    }

    pub async fn watch_path(&self, path: &str) -> anyhow::Result<()> {
        tracing::info!("Watching path: {}", path);
        self.watch_add(path, "5m", None, "section").await
    }

    pub async fn watch_add(
        &self,
        path: &str,
        poll_interval: &str,
        realm_hint: Option<&str>,
        chunk_by: &str,
    ) -> anyhow::Result<()> {
        let config = crate::config::WatchConfig {
            path: path.to_string(),
            chunk_by: chunk_by.to_string(),
            poll_interval: poll_interval.to_string(),
            realm_hint: realm_hint.map(|s| s.to_string()),
        };

        let mut fw_guard = self.file_watcher.write().await;
        if fw_guard.is_none() {
            let (watcher, _rx) = FileWatcher::new();
            watcher.start(self.config.watch.clone()).await?;
            *fw_guard = Some(watcher);
        }
        let watcher = fw_guard.as_ref().unwrap();
        watcher.add_watch(config).await?;
        watcher.persist_watches(&self.config_path).await?;
        Ok(())
    }

    pub async fn watch_list(&self) -> anyhow::Result<()> {
        let fw_guard = self.file_watcher.read().await;
        if let Some(ref watcher) = *fw_guard {
            let watches = watcher.list_watches().await;
            if watches.is_empty() {
                println!("No active watches.");
            } else {
                println!("👁️  Active watched directories ({}):", watches.len());
                println!();
                for (path, chunk, hint, poll) in &watches {
                    println!("  📂 {}", path);
                    println!(
                        "     chunk: {} | poll: {} | realm: {}",
                        chunk,
                        poll,
                        hint.as_deref().unwrap_or("auto")
                    );
                    println!();
                }
            }
        } else if self.config.watch.is_empty() {
            println!("No watches configured.");
        } else {
            println!("👁️  Configured watches ({}):", self.config.watch.len());
            println!();
            for w in &self.config.watch {
                println!("  📂 {}", w.path);
                println!(
                    "     chunk: {} | poll: {} | realm: {}",
                    w.chunk_by,
                    w.poll_interval,
                    w.realm_hint.as_deref().unwrap_or("auto")
                );
                println!();
            }
        }
        Ok(())
    }

    pub async fn watch_remove(&self, path: &str) -> anyhow::Result<()> {
        let fw_guard = self.file_watcher.write().await;
        if let Some(ref watcher) = *fw_guard {
            watcher.remove_watch(path).await?;
            watcher.persist_watches(&self.config_path).await?;
        }
        // Remove from the in-memory config watch list
        let path_normalized = std::path::PathBuf::from(path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(path));
        let mut cfg = self.config.clone();
        cfg.watch.retain(|w| {
            let wp = std::path::PathBuf::from(&w.path);
            wp.canonicalize().unwrap_or(wp) != path_normalized
        });
        Ok(())
    }

    /// Start all configured file watchers and return a receiver for change events.
    /// The caller should spawn a task to process FileChangeEvents.
    pub async fn start_watchers(
        &self,
    ) -> anyhow::Result<Option<tokio::sync::mpsc::UnboundedReceiver<Vec<FileChangeEvent>>>> {
        if self.config.watch.is_empty() {
            tracing::info!("No file watches configured.");
            return Ok(None);
        }

        let mut fw_guard = self.file_watcher.write().await;
        let (watcher, event_rx) = FileWatcher::new();
        watcher.start(self.config.watch.clone()).await?;
        *fw_guard = Some(watcher);

        // Return a transformed receiver: raw paths → change events
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<FileChangeEvent>>();
        let watcher_ref = self.file_watcher.clone();
        tokio::spawn(async move {
            let mut rx = event_rx;
            while let Some(path) = rx.recv().await {
                if let Some(w) = watcher_ref.read().await.as_ref() {
                    let events = w.check_changes(&[path]).await;
                    if !events.is_empty() {
                        let _ = tx.send(events);
                    }
                }
            }
        });

        tracing::info!("✅ Started {} file watchers", self.config.watch.len());
        Ok(Some(rx))
    }

    /// Re-ingest a changed file (used by the daemon's watcher event handler).
    async fn reingest_changed_file(
        &self,
        path: &std::path::Path,
        chunk_by: &str,
        realm_hint: Option<&str>,
    ) -> anyhow::Result<()> {
        let ingester = ingester::Ingester::new(self.config.clone());
        let chunks = ingester
            .ingest_path(&path.to_string_lossy(), chunk_by)
            .await?;

        let embedder = self.make_embedder()?;
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let vectors = embedder.embed_batch(&texts).await?;

        for (i, chunk) in chunks.iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let vector = vectors.get(i).cloned().unwrap_or_default();

            let realm_id = if let Some(hint) = realm_hint {
                self.store.find_realm_by_name(hint).await?.map(|r| r.id)
            } else {
                None
            };

            let realm_id = match realm_id {
                Some(rid) => rid,
                None => self.auto_assign_realm(&vector).await?,
            };

            let realm = self.store.get_realm(&realm_id).await?;
            let realm_name = realm.map(|r| r.name.clone()).unwrap_or_default();

            // Infer reaction score from content
            let reaction_score = crate::engine::reactions::infer_reaction(&chunk.content);

            self.store
                .store_memory(
                    &id,
                    &vector,
                    &chunk.content,
                    chunk.heading.as_deref(),
                    Some(chunk.source_file.as_str()),
                    &realm_id,
                    &realm_name,
                    &chunk.source_hash,
                    &chunk.chunk_type,
                    reaction_score,
                )
                .await?;
        }

        tracing::info!(
            "  ✅ Re-ingested {} chunks from {}",
            chunks.len(),
            path.display()
        );
        Ok(())
    }

    /// Process file change events (for use by the daemon).
    pub async fn handle_watch_events(
        &self,
        mut event_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<FileChangeEvent>>,
    ) -> anyhow::Result<()> {
        while let Some(events) = event_rx.recv().await {
            for event in events {
                match event {
                    FileChangeEvent::Modified {
                        path, watch_config, ..
                    }
                    | FileChangeEvent::Created {
                        path, watch_config, ..
                    } => {
                        let chunk_by = watch_config.chunk_by;
                        let realm_hint = watch_config.realm_hint;
                        if let Err(e) = self
                            .reingest_changed_file(&path, &chunk_by, realm_hint.as_deref())
                            .await
                        {
                            tracing::error!("Failed to re-ingest {}: {}", path.display(), e);
                        }
                    }
                    FileChangeEvent::Deleted { path, .. } => {
                        tracing::info!("🗑️  File deleted from watch: {}", path.display());
                        // TODO: Optionally delete corresponding memories
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn search(
        &self,
        query: &str,
        realm: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        let embedder = self.make_embedder()?;
        let query_vector = embedder.embed(query).await?;

        // Try TurboVec search first (fast, compressed) when available and no tag filters
        let results = if tags.is_none() || tags.as_ref().map_or(true, |t| t.is_empty()) {
            let tv_guard = self.turbovec_index.read().await;
            if let Some(ref tv) = *tv_guard {
                // TurboVec path: search compressed index, fetch payloads from Qdrant
                let fetch_limit = (limit + offset) * 4; // Over-fetch to account for filtering
                let (scores, indices) = tv.search(&query_vector, fetch_limit);
                let ids = tv.resolve_ids(&indices);

                // Fetch full payloads from Qdrant by ID
                let mut fetched: Vec<_> = Vec::new();
                for (i, &id) in ids.iter().enumerate() {
                    if i >= scores.len() { break; }
                    if let Ok(Some(point)) = self.store.get_memory(id).await {
                        let score = scores[i];
                        if score >= min_score {
                            if realm.map_or(true, |re| point.realm_name == re) {
                                fetched.push((score, point));
                            }
                        }
                    }
                }
                fetched.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                fetched
            } else {
                // TurboVec not loaded — fall back to Qdrant
                let qr = if let Some(tags) = tags {
                    if tags.is_empty() {
                        self.store.search(&query_vector, limit + offset, min_score, realm).await?
                    } else {
                        self.store.search_by_tags(&query_vector, tags, limit + offset).await?
                            .into_iter()
                            .filter(|r| r.score >= min_score)
                            .filter(|r| realm.map_or(true, |re| r.payload.realm_name == re))
                            .collect()
                    }
                } else {
                    self.store.search(&query_vector, limit + offset, min_score, realm).await?
                };
                qr.into_iter().map(|r| (r.score, r.payload)).collect()
            }
        } else {
            // Tag filters active — must use Qdrant (TurboVec has no tag support)
            let qr = self.store.search_by_tags(&query_vector, tags.as_ref().unwrap(), limit + offset).await?
                .into_iter()
                .filter(|r| r.score >= min_score)
                .filter(|r| realm.map_or(true, |re| r.payload.realm_name == re))
                .collect::<Vec<_>>();
            qr.into_iter().map(|r| (r.score, r.payload)).collect()
        };

        // Phase A.2: soft down-weight by verification status. Applies to ALL
        // retrieval paths above (TurboVec, Qdrant fallback, tag-filter).
        // Never filters — only re-weights, then re-sorts so the weighted
        // ranking is reflected before pagination.
        let mut results: Vec<(f32, crate::storage::qdrant::MemoryPoint)> = results
            .into_iter()
            .map(|(score, p)| {
                (
                    score * verification_score_multiplier(&p.verification_status),
                    p,
                )
            })
            .collect();
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Apply pagination
        let results: Vec<_> = results.into_iter().skip(offset).take(limit).collect();

        // Touch all returned memories (increment access_count, bump importance)
        let touch_ids: Vec<String> = results.iter().map(|(_, p)| p.id.clone()).collect();
        let bump = self.config.slumber.touch_importance_bump;
        let touch_refs: Vec<&str> = touch_ids.iter().map(|s| s.as_str()).collect();
        let _ = self.store.track_access_batch(&touch_refs, bump).await;

        // Spreading activation: bump associated memories too
        let spread_bump = self.config.slumber.spreading_activation_bump;
        if spread_bump > 0.0 {
            let associated_ids: Vec<&str> = results
                .iter()
                .flat_map(|(_, p)| p.related_memory_ids.iter().map(|s| s.as_str()))
                .collect();
            if !associated_ids.is_empty() {
                let _ = self
                    .store
                    .track_access_batch(&associated_ids, spread_bump)
                    .await;
            }
        }

        {
            let mut state = self.slumber_state.write().await;
            state.last_query = chrono::Utc::now();
        }
        self.touch_activity().await;

        Ok(results
            .into_iter()
            .map(|(score, p)| MemoryResult {
                id: p.id,
                content: p.content,
                heading: p.heading,
                realm_name: p.realm_name,
                importance: p.importance,
                score,
                last_accessed: p.last_accessed,
                access_count: p.access_count,
                upvotes: p.upvotes,
                related_memory_ids: p.related_memory_ids,
                association_strengths: p.association_strengths,
            })
            .collect())
    }

    pub async fn get_memory(
        &self,
        id: &str,
    ) -> anyhow::Result<crate::storage::qdrant::MemoryPoint> {
        self.store
            .get_memory(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Memory not found: {}", id))
    }

    pub async fn recall(
        &self,
        limit: usize,
        realm: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        // Scroll all memories and sort by importance × recency score
        let memories = self.store.scroll_all_memories().await?;

        let now = chrono::Utc::now().timestamp() as f64;
        let mut scored: Vec<_> = memories
            .into_iter()
            .filter(|m| realm.as_deref().map_or(true, |r| m.realm_name == r))
            .map(|m| {
                // Recency decay: 1/(1 + days_since_access * 0.1)
                let access_ts = chrono::DateTime::parse_from_rfc3339(&m.last_accessed)
                    .map(|dt| dt.timestamp() as f64)
                    .unwrap_or(0.0);
                let days_since = ((now - access_ts) / 86400.0).max(0.0);
                let recency = 1.0 / (1.0 + days_since * 0.1);
                // Phase A.2: soft verification down-weight (never filters)
                let score = m.importance
                    * recency as f32
                    * (1.0 + m.access_count as f32 * 0.05)
                    * verification_score_multiplier(&m.verification_status);
                (m, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Touch the top memories before returning them
        let top: Vec<_> = scored.iter().take(limit).collect();
        let touch_ids: Vec<&str> = top.iter().map(|(m, _)| m.id.as_str()).collect();
        let bump = self.config.slumber.touch_importance_bump;
        let _ = self.store.track_access_batch(&touch_ids, bump).await;

        // Spreading activation: bump associated memories too
        let spread_bump = self.config.slumber.spreading_activation_bump;
        if spread_bump > 0.0 {
            let associated_ids: Vec<&str> = top
                .iter()
                .flat_map(|(m, _)| m.related_memory_ids.iter().map(|s| s.as_str()))
                .collect();
            if !associated_ids.is_empty() {
                let _ = self
                    .store
                    .track_access_batch(&associated_ids, spread_bump)
                    .await;
            }
        }

        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(m, score)| MemoryResult {
                id: m.id,
                content: m.content,
                heading: m.heading,
                realm_name: m.realm_name,
                importance: m.importance,
                score,
                last_accessed: m.last_accessed,
                access_count: m.access_count,
                upvotes: m.upvotes,
                related_memory_ids: m.related_memory_ids,
                association_strengths: m.association_strengths,
            })
            .collect())
    }

    /// List all memories with optional realm filter and sort, without recency weighting.
    pub async fn list_memories(
        &self,
        realm: Option<&str>,
        sort: &str,
        descending: bool,
    ) -> anyhow::Result<Vec<crate::storage::qdrant::MemoryPoint>> {
        let all = self.store.scroll_all_memories().await?;

        // Filter by realm if specified
        let filtered: Vec<_> = match realm {
            Some(r) => all.into_iter().filter(|m| &m.realm_name == r).collect(),
            None => all,
        };

        // Sort
        let mut sorted = filtered;
        match sort {
            "importance" => sorted.sort_by(|a, b| {
                let cmp = a.importance.partial_cmp(&b.importance).unwrap();
                if descending { cmp } else { cmp.reverse() }
            }),
            "last_accessed" => sorted.sort_by(|a, b| {
                let cmp = a.last_accessed.cmp(&b.last_accessed);
                if descending { cmp } else { cmp.reverse() }
            }),
            "access_count" => sorted.sort_by(|a, b| {
                let cmp = a.access_count.cmp(&b.access_count);
                if descending { cmp } else { cmp.reverse() }
            }),
            _ => sorted.sort_by(|a, b| {
                let cmp = a.ingested_at.cmp(&b.ingested_at);
                if descending { cmp } else { cmp.reverse() }
            }),
        }
        Ok(sorted)
    }

    /// Get the most commonly used tags.
    pub async fn get_tag_suggestions(&self, limit: usize) -> anyhow::Result<Vec<(String, u32)>> {
        self.store.get_tag_suggestions(limit).await
    }

    pub async fn list_realms(&self) -> anyhow::Result<Vec<crate::storage::qdrant::RealmPoint>> {
        self.store.list_realms().await
    }

    pub async fn create_realm(&self, name: &str, description: Option<&str>) -> anyhow::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        // Use a zero vector as initial centroid; will be updated during slumber
        let centroid = vec![0.0; self.config.embedding.dimensions as usize];
        self.store
            .store_realm(&id, &centroid, name, description, true)
            .await
    }

    pub async fn show_realm(
        &self,
        name: &str,
    ) -> anyhow::Result<crate::storage::qdrant::RealmPoint> {
        self.store
            .find_realm_by_name(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Realm not found: {}", name))
    }

    pub async fn merge_realms(&self, target: &str, source: &str) -> anyhow::Result<()> {
        let target_realm = self
            .store
            .find_realm_by_name(target)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Target realm not found: {}", target))?;
        let source_realm = self
            .store
            .find_realm_by_name(source)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Source realm not found: {}", source))?;

        tracing::info!("Merging realm '{}' into '{}'", source, target);

        // Reassign all memories from source to target realm
        let all = self.store.scroll_all_memories().await?;
        for mem in all {
            if mem.realm_id.as_deref() == Some(&source_realm.id) {
                let payload: qdrant_client::Payload = serde_json::json!({
                    "realm_id": target_realm.id,
                    "realm_name": target_realm.name,
                })
                .try_into()
                .unwrap_or_default();
                self.store.update_memory_payload(&mem.id, payload).await?;
            }
        }

        // Update realm counts
        self.store.update_realm_counts().await?;

        // Delete source realm
        self.store.delete_realm(&source_realm.id).await?;

        Ok(())
    }

    pub async fn split_realm(&self, name: &str) -> anyhow::Result<()> {
        // TODO: k-means split with auto-generated names
        tracing::info!("Splitting realm: {}", name);
        Ok(())
    }

    pub async fn upvote(&self, id: &str) -> anyhow::Result<()> {
        let memory = self.get_memory(id).await?;
        let new_upvotes = memory.upvotes + 1;
        let new_importance = (memory.importance + 0.1).min(1.0);
        self.store
            .update_upvotes(id, new_upvotes, new_importance)
            .await
    }

    pub async fn downvote(&self, id: &str) -> anyhow::Result<()> {
        let memory = self.get_memory(id).await?;
        let new_upvotes = (memory.upvotes as i64 - 1).max(0) as u32;
        let new_importance = (memory.importance - 0.1).max(0.01);
        self.store
            .update_upvotes(id, new_upvotes, new_importance)
            .await
    }

    pub async fn prune_queue(&self) -> anyhow::Result<Vec<crate::storage::qdrant::MemoryPoint>> {
        let memories = self.store.scroll_all_memories().await?;
        let now = chrono::Utc::now();
        let auto_archive_days = self.config.slumber.auto_archive_days as i64;
        let prune_threshold = self.config.slumber.prune_threshold;

        Ok(memories
            .into_iter()
            .filter(|m| {
                // Low importance AND (old OR no access)
                if m.importance > prune_threshold {
                    return false;
                }
                let age_ok = chrono::DateTime::parse_from_rfc3339(&m.ingested_at)
                    .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() > auto_archive_days)
                    .unwrap_or(false);
                let no_access = m.access_count == 0;
                age_ok || no_access
            })
            .collect())
    }

    pub async fn archive_memory(&self, id: &str) -> anyhow::Result<()> {
        // Reduce importance to near-zero so slumber will prune it
        self.store.update_upvotes(id, 0, 0.01).await
    }

    pub async fn delete_memory(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_memory(id).await
    }

    pub async fn edit_memory(&self, id: &str, new_content: &str) -> anyhow::Result<()> {
        // Re-embed the new content and update the point
        let embedder = self.make_embedder()?;
        let vector = embedder.embed(new_content).await?;

        let existing = self.get_memory(id).await?;
        let realm_id = existing.realm_id.as_deref().unwrap_or("");

        // Infer reaction score from new content
        let reaction_score = crate::engine::reactions::infer_reaction(new_content);

        self.store
            .store_memory(
                id,
                &vector,
                new_content,
                existing.heading.as_deref(),
                existing.source_file.as_deref(),
                realm_id,
                &existing.realm_name,
                &existing.source_hash,
                &existing.chunk_type,
                reaction_score,
            )
            .await?;

        tracing::info!("Updated memory {}", id);
        Ok(())
    }

    pub async fn slumber_status(&self) -> SlumberStatus {
        let state = self.slumber_state.read().await;
        SlumberStatus {
            state: state.status.clone(),
            last_run: state.last_run.map(|t| t.to_rfc3339()),
            next_scheduled: None,
            memories_processed: state.memories_processed,
            realms_reorganized: state.realms_reorganized,
            last_report: state.last_report.clone(),
        }
    }

    pub async fn trigger_slumber(
        &self,
        force_consolidation: bool,
    ) -> anyhow::Result<slumber::SlumberReport> {
        {
            let mut state = self.slumber_state.write().await;
            state.status = "running".into();
        }

        tracing::info!("💤 Slumber started...");
        let slumber = slumber::SlumberEngine::new(self.config.clone(), self.store.clone_store());
        let report = slumber.run_full_pipeline(force_consolidation).await?;

        {
            let mut state = self.slumber_state.write().await;
            state.status = "idle".into();
            state.last_run = Some(chrono::Utc::now());
            state.realms_reorganized += 1;
            state.last_report = Some(report.clone());
        }
        tracing::info!("✅ Slumber complete.");
        Ok(report)
    }

    pub async fn pause_slumber(&self) {
        let mut state = self.slumber_state.write().await;
        state.status = "paused".into();
    }

    pub async fn resume_slumber(&self) {
        let mut state = self.slumber_state.write().await;
        state.status = "idle".into();
    }

    pub async fn stats(&self) -> anyhow::Result<SystemStats> {
        let mem_stats = self
            .store
            .get_collection_stats(&self.config.qdrant.collection_memories)
            .await?;
        let realms = self.store.list_realms().await?;
        let slumber = self.slumber_status().await;

        Ok(SystemStats {
            total_memories: mem_stats.vector_count,
            total_realms: realms.len() as u32,
            storage_bytes: mem_stats.size_bytes,
            embedding_provider: self.embed_provider.clone(),
            embedding_model: self.embed_model.clone(),
            embedding_dimensions: self.embed_dimensions,
            slumber_state: slumber.state,
        })
    }

    /// Count memories by verification status (Phase A.2).
    /// Backs `GET /api/v1/memories/verification-summary`.
    pub async fn verification_summary(
        &self,
    ) -> anyhow::Result<crate::storage::qdrant::VerificationStatusCounts> {
        self.store.count_by_verification_status().await
    }

    pub async fn store_memory(
        &self,
        content: &str,
        _tags: Option<Vec<String>>,
        realm_hint: Option<&str>,
        source: Option<&str>,
    ) -> anyhow::Result<String> {
        let embedder = self.make_embedder()?;
        let vector = embedder.embed(content).await?;

        let id = uuid::Uuid::new_v4().to_string();

        // Assign to realm
        let realm_id = if let Some(hint) = realm_hint {
            self.store.find_realm_by_name(hint).await?.map(|r| r.id)
        } else {
            None
        };

        let realm_id = match realm_id {
            Some(rid) => rid,
            None => self.auto_assign_realm(&vector).await?,
        };

        let realm = self.store.get_realm(&realm_id).await?;
        let realm_name = realm.map(|r| r.name.clone()).unwrap_or_default();

        let source_str = source.unwrap_or("manual");

        // Infer reaction score from content
        let reaction_score = crate::engine::reactions::infer_reaction(content);

        self.store
            .store_memory(
                &id,
                &vector,
                content,
                None,
                Some(source_str),
                &realm_id,
                &realm_name,
                "",
                "manual",
                reaction_score,
            )
            .await?;

        Ok(id)
    }

    pub async fn graph_search(
        &self,
        entity: &str,
        _relationship: Option<&str>,
        _depth: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        // TODO: implement knowledge graph traversal
        // For now, fall back to semantic search and wrap results
        let embedder = self.make_embedder()?;
        let query_vector = embedder.embed(entity).await?;
        let results = self.store.search(&query_vector, 10, 0.3, None).await?;

        let output: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "entity": r.payload.heading.as_ref().unwrap_or(&String::new()),
                    "realm": r.payload.realm_name,
                    "score": r.score,
                    "depth": 0,
                })
            })
            .collect();

        Ok(output)
    }

    pub async fn export(&self, path: &str) -> anyhow::Result<()> {
        // Export with vectors so they can be reused on import
        let memories = self.store.scroll_all_memories_with_vectors().await?;
        let json = serde_json::to_string_pretty(&memories)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub async fn import(&self, path: &str, reuse_vectors: bool) -> anyhow::Result<usize> {
        let content = std::fs::read_to_string(path)?;

        if reuse_vectors {
            // Try to import with vectors first
            if let Ok(memories) = serde_json::from_str::<Vec<MemoryWithVector>>(&content) {
                let count = memories.len();
                for m in &memories {
                    let realm_id = m.memory.realm_id.as_deref().unwrap_or("");
                    let reaction_score = crate::engine::reactions::infer_reaction(&m.memory.content);
                    self.store
                        .store_memory(
                            &m.memory.id,
                            &m.vector,
                            &m.memory.content,
                            m.memory.heading.as_deref(),
                            m.memory.source_file.as_deref(),
                            realm_id,
                            &m.memory.realm_name,
                            &m.memory.source_hash,
                            &m.memory.chunk_type,
                            reaction_score,
                        )
                        .await?;
                }
                tracing::info!("Imported {} memories with vectors from {}", count, path);
                return Ok(count);
            }
        }

        // Fallback: import without vectors, re-embed
        let memories: Vec<crate::storage::qdrant::MemoryPoint> = serde_json::from_str(&content)?;
        let count = memories.len();

        let embedder = self.make_embedder()?;
        for mem in &memories {
            let vector = embedder.embed(&mem.content).await?;
            let realm_id = mem.realm_id.as_deref().unwrap_or("");
            let reaction_score = crate::engine::reactions::infer_reaction(&mem.content);
            self.store
                .store_memory(
                    &mem.id,
                    &vector,
                    &mem.content,
                    mem.heading.as_deref(),
                    mem.source_file.as_deref(),
                    realm_id,
                    &mem.realm_name,
                    &mem.source_hash,
                    &mem.chunk_type,
                    reaction_score,
                )
                .await?;
        }

        tracing::info!("Imported {} memories from {}", count, path);
        Ok(count)
    }

    /// Return the config file path.
    pub fn config_path(&self) -> &str {
        &self.config_path
    }

    /// Set the config file path (call after loading from a specific path).
    pub fn set_config_path(&mut self, path: &str) {
        self.config_path = path.to_string();
    }

    /// Get a clone of the config.
    pub fn config(&self) -> AppConfig {
        self.config.clone()
    }

    /// Get the store reference.
    pub fn store(&self) -> &QdrantStore {
        &self.store
    }

    /// List all gaps, optionally filtered by status.
    pub async fn list_gaps(
        &self,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<crate::storage::qdrant::GapPoint>> {
        self.store.list_gaps(status).await
    }

    /// Resolve a gap (mark as resolved).
    pub async fn resolve_gap(&self, gap_id: &str) -> anyhow::Result<()> {
        self.store.update_gap_status(gap_id, "resolved").await
    }

    /// Dismiss a gap (mark as dismissed).
    pub async fn dismiss_gap(&self, gap_id: &str) -> anyhow::Result<()> {
        self.store.update_gap_status(gap_id, "dismissed").await
    }

    /// Infer gaps and suggestions based on a topic or memory_id.
    ///
    /// If `topic` is provided, searches for memories matching that topic and
    /// returns gap suggestions based on the related cluster. If `memory_id` is
    /// provided, analyzes that specific memory for gaps. If neither is provided,
    /// returns general top gaps.
    pub async fn infer_gaps(
        &self,
        topic: Option<&str>,
        memory_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::api::routes::inference::GapSuggestion>> {
        use crate::api::routes::inference::GapSuggestion;

        // If a specific memory_id is given, use its realm/cluster context
        let target_cluster_id = if let Some(mem_id) = memory_id {
            let mem = self.get_memory(mem_id).await?;
            mem.topic_clusters.first().cloned()
        } else if let Some(t) = topic {
            // Search for memories matching the topic and find the most relevant cluster
            let _results = self
                .search(t, None, None, 5, 0, 0.3)
                .await?;
            // Collect all memories and their topic clusters
            let all_memories = self.store.scroll_all_memories().await?;
            let mut cluster_memories: std::collections::HashMap<String, Vec<&crate::storage::qdrant::MemoryPoint>> =
                std::collections::HashMap::new();
            for mem in &all_memories {
                for cluster_id in &mem.topic_clusters {
                    cluster_memories
                        .entry(cluster_id.clone())
                        .or_default()
                        .push(mem);
                }
            }
            // Find cluster with most topic-keyword matches
            let topic_lower = t.to_lowercase();
            let mut best_cluster: Option<String> = None;
            let mut best_score = 0usize;
            for (cid, mems) in &cluster_memories {
                let score: usize = mems
                    .iter()
                    .filter(|m| m.content.to_lowercase().contains(&topic_lower))
                    .count();
                if score > best_score {
                    best_score = score;
                    best_cluster = Some(cid.clone());
                }
            }
            best_cluster
        } else {
            None
        };

        // Fetch all open gaps
        let all_gaps = self.store.list_gaps(Some("open")).await?;

        // Filter/sort gaps by relevance
        let mut suggestions: Vec<GapSuggestion> = all_gaps
            .into_iter()
            .filter(|g| {
                if let Some(ref tc) = target_cluster_id {
                    g.cluster_id == *tc || g.related_memory_ids.iter().any(|id| id == tc)
                } else {
                    true
                }
            })
            .map(|g| GapSuggestion {
                id: g.id,
                gap_type: g.gap_type,
                suggested_topic: g.suggested_topic,
                description: g.description,
                confidence: g.importance,
                related_memory_ids: g.related_memory_ids,
                suggested_search_queries: g.suggested_search_queries,
                importance: g.importance,
                created_at: g.created_at,
            })
            .collect();

        // Sort by importance (confidence) descending
        suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
        suggestions.truncate(limit);

        Ok(suggestions)
    }

    /// Run session-end extraction: LLM-assisted extraction of decisions, follow-ups,
    /// and insights from a conversation session. Results are stored as high-importance
    /// memories in a dedicated realm.
    pub async fn run_session_end(
        &self,
        input: crate::engine::session::SessionInput,
    ) -> anyhow::Result<crate::engine::session::SessionReport> {
        let session_engine =
            crate::engine::session::SessionEngine::new(self.config.clone(), self.store.clone());
        session_engine.run_session_end(input).await
    }

    // ─── Knowledge Graph ─────────────────────────────────────────────────────────

    /// Build the knowledge graph from all stored memories.
    /// Extracts entities and creates edges between related memories.
    pub async fn build_graph(&self, similarity_threshold: f32) -> anyhow::Result<usize> {
        // Clear existing edges first
        self.store.delete_all_graph_edges().await?;

        let graph = graph::KnowledgeGraph::new(self.store.clone());
        graph.build_graph(similarity_threshold).await
    }

    /// Traverse the knowledge graph from a starting memory using BFS.
    /// Returns connected memories with path information.
    pub async fn graph_traverse(
        &self,
        memory_id: &str,
        depth: usize,
    ) -> anyhow::Result<Vec<graph::GraphTraversalResult>> {
        let graph = graph::KnowledgeGraph::new(self.store.clone());
        graph.search_graph(memory_id, depth).await
    }

    /// Get directly connected memories for a given memory.
    pub async fn graph_neighbors(
        &self,
        memory_id: &str,
    ) -> anyhow::Result<Vec<graph::Relationship>> {
        let graph = graph::KnowledgeGraph::new(self.store.clone());
        graph.get_neighbors(memory_id).await
    }

    /// Get statistics about the knowledge graph.
    pub async fn graph_stats(&self) -> anyhow::Result<graph::GraphStats> {
        let graph = graph::KnowledgeGraph::new(self.store.clone());
        graph.get_stats().await
    }

    // ─── Backup/Restore ────────────────────────────────────────────────────────

    /// Backup all memories, realms, and graph edges to a timestamped tarball.
    pub async fn backup(&self, output_path: Option<&str>) -> anyhow::Result<String> {
        backup::backup(self, output_path).await
    }

    /// Restore memories, realms, and graph edges from a backup tarball.
    pub async fn restore(&self, backup_path: &str, force: bool) -> anyhow::Result<usize> {
        backup::restore(self, backup_path, force).await
    }

    /// List available backups sorted by date (newest first).
    pub fn list_backups(&self, backup_dir: Option<&str>) -> anyhow::Result<Vec<backup::BackupInfo>> {
        backup::list_backups(backup_dir)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::verification_score_multiplier;

    #[test]
    fn test_verification_score_multiplier_values() {
        assert_eq!(verification_score_multiplier("verified"), 1.0);
        assert_eq!(verification_score_multiplier("unverified"), 0.95);
        assert_eq!(verification_score_multiplier("stale"), 0.85);
        assert_eq!(verification_score_multiplier("contradicted"), 0.5);
    }

    #[test]
    fn test_verification_score_multiplier_unknown_defaults_to_unverified() {
        // Never-stamped ("") or unexpected statuses get the slight unverified
        // penalty — never exclusion, never a boost.
        assert_eq!(verification_score_multiplier(""), 0.95);
        assert_eq!(verification_score_multiplier("something-else"), 0.95);
    }

    #[test]
    fn test_verification_downweight_orders_results() {
        // Regression: two identical memories, one contradicted — the verified
        // one must rank first after weighting.
        let raw_score = 0.9f32;
        let verified = raw_score * verification_score_multiplier("verified");
        let contradicted = raw_score * verification_score_multiplier("contradicted");
        assert!(verified > contradicted);
        let stale = raw_score * verification_score_multiplier("stale");
        assert!(verified > stale);
        assert!(stale > contradicted);
    }
}
