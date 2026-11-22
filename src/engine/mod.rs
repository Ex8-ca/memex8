pub mod associations;
pub mod backup;
pub mod chunker;
pub mod compressed_index;
pub mod consolidation;
pub mod coverage;
pub mod decay;
pub mod dedup;
pub mod dedup_removal;
pub mod doctor;
pub mod dream;
pub mod embedder;
pub mod evidence;
pub mod graph;
pub mod ingester;
pub mod lexical;
pub mod memex8_md;
pub mod person_lookup;
pub mod providers;
pub mod quantizer;
pub mod query_intent;
pub mod reactions;
pub mod redaction;
pub mod retrieval;
pub mod scheduler;
pub mod session;
pub mod slumber;
pub mod slumber_pause;
pub mod timeline;
pub mod vector_math;
pub mod watcher;
pub mod write;
pub mod write_check;

use crate::config::AppConfig;
pub use crate::engine::watcher::{FileChangeEvent, FileWatcher};
use crate::storage::qdrant::{MemoryWithVector, QdrantStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use vector_math::cosine_similarity;
pub use write::{Dedup, NewMemory, Visibility};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResult {
    pub id: String,
    pub content: String,
    // Projection metadata stays internal so legacy full responses keep their shape.
    #[serde(skip)]
    pub(crate) summary: Option<String>,
    #[serde(skip)]
    pub(crate) ingested_at: String,
    #[serde(skip)]
    pub(crate) memory_type: String,
    #[serde(skip)]
    pub(crate) verification_status: String,
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
        "certified" => 1.05,
        "verified" => 1.0,
        "unverified" => 0.95, // slight penalty, not exclusion
        "stale" => 0.85,
        "contradicted" => 0.5,
        _ => 0.95,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlumberStatus {
    /// "idle", "running" or "paused". `paused` is derived from the persisted
    /// flag, not from the last pipeline transition, so it cannot be overwritten
    /// by a finishing (or failing) run.
    pub state: String,
    /// Persisted operator pause. A paused engine refuses both scheduled and
    /// explicit triggers.
    #[serde(default)]
    pub paused: bool,
    /// Where the pause flag lives (makes an out-of-band pause auditable).
    #[serde(default)]
    pub pause_file: String,
    pub last_run: Option<String>,
    pub next_scheduled: Option<String>,
    pub memories_processed: u64,
    pub realms_reorganized: u32,
    pub last_report: Option<slumber::SlumberReport>,
}

/// Slumber is paused; the caller must resume it before triggering.
///
/// A distinct type (rather than a bare `anyhow!` string) lets the HTTP layer
/// answer 409 and the CLI report the real reason instead of failing generically.
#[derive(Debug)]
pub struct SlumberPausedError;
impl std::fmt::Display for SlumberPausedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("slumber is paused; resume it before triggering")
    }
}
impl std::error::Error for SlumberPausedError {}

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
    /// TurboVec compressed vector index for fast search; see `compressed_index`.
    index: compressed_index::CompressedIndex,
    /// Per-content-hash locks for `write_memory` with `Dedup::Identical`, so two identical
    /// concurrent writes cannot both miss the existing-memory lookup.
    idempotency_locks: IdempotencyLocks,
    /// BM25 keyword index (a cache rebuilt from Qdrant; see `lexical`).
    lexical: Arc<RwLock<lexical::LexicalIndex>>,
    /// Dreams live in their own files, outside the memories collection.
    dreams: dream::DreamStore,
    dream_lock: Arc<tokio::sync::Mutex<()>>,
    /// Operator pause for scheduled and explicit slumber runs.
    pause: slumber_pause::SlumberPause,
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
            tracing::info!(
                "Using OpenAI-compatible embeddings: {} at {} ({}d)",
                model,
                openai_base_url,
                dimensions
            );
        } else {
            tracing::info!("Using Ollama embeddings: {} ({}d)", model, dimensions);
        }

        store.ensure_collections(dimensions).await?;

        // Try to load TurboVec index from disk for fast search
        let index =
            compressed_index::CompressedIndex::load(&config, &store, dimensions as usize).await;

        // Dreams sit next to the TurboVec index, so they're on the data volume and in backups.
        let dreams = dream::DreamStore::new(
            std::path::Path::new(&config.turbovec.index_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("dreams"),
        );
        let pause = slumber_pause::SlumberPause::for_config(&config);
        if pause.is_paused() {
            tracing::warn!(
                "⏸️  Slumber is paused in {}; scheduled runs are refused until resumed",
                pause.path()
            );
        }
        let engine = Self {
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
            index,
            idempotency_locks: Default::default(),
            lexical: Arc::new(RwLock::new(lexical::LexicalIndex::default())),
            dreams,
            dream_lock: Arc::new(tokio::sync::Mutex::new(())),
            pause,
        };
        // Reconcile before loading any searchable index or exposing this engine.
        engine.reconcile_pending_erasures().await?;
        if let Err(e) = engine.rebuild_lexical_index().await {
            tracing::warn!(
                "Lexical index not built ({e}); search stays vector-only until the next rebuild"
            );
        }
        Ok(engine)
    }

    pub fn dream_store(&self) -> &dream::DreamStore {
        &self.dreams
    }

    /// Keep a dream. With `remember`, also store it as a `dream`-type memory (the one
    /// explicit, user-requested path from a dream to the memories collection). Keeping
    /// twice with `remember` never stores it twice.
    pub async fn keep_dream(
        &self,
        id: &str,
        remember: bool,
    ) -> anyhow::Result<Option<dream::Dream>> {
        let Some(d) = self.dreams.get(id) else {
            return Ok(None);
        };
        if !remember || d.remembered_as.is_some() {
            return self.dreams.keep(id);
        }
        let memory_id = self
            .write_memory(NewMemory {
                tags: vec![dream::DREAM_TYPE.to_string()],
                source: Some("dream"),
                memory_type: Some(dream::DREAM_TYPE),
                ..NewMemory::new(&dream::memory_text_for(&d))
            })
            .await?
            .id;
        self.dreams.keep_remembered(id, &memory_id)
    }

    pub fn dream_config(&self) -> &crate::config::DreamConfig {
        &self.config.dream
    }

    /// Generate one dream now (also used by the schedule). It is saved as *pending*
    /// for the user to keep or discard. This never writes to the memories collection.
    pub async fn dream_now(&self) -> anyhow::Result<dream::Dream> {
        let _guard = self
            .dream_lock
            .try_lock()
            .map_err(|_| dream::DreamError::InProgress)?;
        let cfg = &self.config.dream;
        let expired = self
            .dreams
            .expire_pending(cfg.pending_days as i64, chrono::Utc::now());
        if expired > 0 {
            tracing::info!("🌙 Expired {} unreviewed dream(s)", expired);
        }
        let all = self.store.scroll_all_memories().await?;
        let seeds = {
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::from_entropy();
            dream::pick_seeds(&all, cfg.seeds, chrono::Utc::now(), &mut rng)
        };
        if seeds.len() < dream::MIN_SEEDS {
            return Err(dream::DreamError::NotEnoughMemories(seeds.len()).into());
        }
        let (raw, model) = dream::call_llm(
            &self.config,
            &dream::system_prompt(cfg.max_words),
            &dream::seed_lines(&seeds),
        )
        .await?;
        let parsed = dream::parse_dream(&raw, &seeds, cfg.max_words)?;
        let d = dream::Dream::new(parsed, seeds.iter().map(|m| m.id.clone()).collect(), &model);
        self.dreams.save(&d)?;
        tracing::info!("🌙 Dream {}: {}", &d.id[..8], d.title);
        Ok(d)
    }

    /// Rebuild the BM25 keyword index from every memory in Qdrant.
    pub async fn rebuild_lexical_index(&self) -> anyhow::Result<usize> {
        let all = self.store.scroll_all_memories().await?;
        let mut fresh = lexical::LexicalIndex::default();
        for m in &all {
            fresh.upsert(&m.id, &lexical::memory_text(m));
        }
        let n = fresh.len();
        *self.lexical.write().await = fresh;
        tracing::info!("🔤 Lexical index: {} memories", n);
        Ok(n)
    }

    /// Load TurboVec index from disk if it exists, otherwise return empty.
    ///
    /// A persisted pair is only trusted when it can be *proven* to describe the
    /// live corpus, which takes four checks:
    ///
    /// 1. the two files are self-consistent (`.tv` slot count == id-map length),
    /// 2. the sidecar meta describes exactly these two files (byte digests),
    /// 3. the corpus fingerprint in that meta equals the live corpus content,
    /// 4. the ID set is exactly the live ID set.
    ///
    /// Every other outcome — missing files, a poisoned marker left by a failed
    /// invalidation, an unreadable or mixed pair, a fingerprint mismatch, or a
    /// stale ID set — is discarded and search stays on the (complete) Qdrant path
    /// until the next slumber rebuilds the index.
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
    ) -> anyhow::Result<ingester::IngestReport> {
        let ingester = ingester::Ingester::new(self.config.clone());
        let outcome = ingester.ingest_path(path, chunk_by).await?;
        tracing::info!(
            "Ingest {}: attempted={} stored={} skipped={} failed={} ignored={} chunks={}",
            path,
            outcome.report.attempted,
            outcome.report.stored,
            outcome.report.skipped,
            outcome.report.failed,
            outcome.report.ignored,
            outcome.chunks.len()
        );
        self.persist_ingested_chunks(outcome.chunks, realm_hint, outcome.report)
            .await
    }

    /// Persist parsed chunks, recording the outcome of each one.
    ///
    /// A chunk-level failure (store error, realm lookup error) is logged and
    /// counted so the caller learns the ingestion was partial; only a batch
    /// embedding failure — which stores nothing — is returned as an error.
    async fn persist_ingested_chunks(
        &self,
        chunks: Vec<ingester::RawChunk>,
        realm_hint: Option<&str>,
        mut report: ingester::IngestReport,
    ) -> anyhow::Result<ingester::IngestReport> {
        if chunks.is_empty() {
            report.finalize_store();
            return Ok(report);
        }

        let embedder = self.make_embedder()?;
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let vectors = embedder.embed_batch(&texts).await?;

        // A loaded compressed index cannot answer for memories that did not exist
        // when it was built, so it would hide every chunk ingested here: the
        // writes happen between two invalidations, like `write_memory`.
        self.index
            .around_write(async {
                for (i, chunk) in chunks.iter().enumerate() {
                    let vector = vectors.get(i).cloned().unwrap_or_default();
                    match self.store_one_chunk(chunk, &vector, realm_hint).await {
                        Ok(id) => {
                            report.note_chunk_stored(&chunk.source_file);
                            let mut state = self.slumber_state.write().await;
                            state.memories_processed += 1;
                            // Keep the keyword index in step with the dense one, as
                            // `store_memory` does; it is rebuilt from Qdrant on the
                            // next slumber anyway.
                            self.lexical.write().await.upsert(&id, &chunk.content);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to store chunk from {}: {}",
                                chunk.source_file,
                                e
                            );
                            report.note_chunk_failed(&chunk.source_file);
                        }
                    }
                }
            })
            .await;

        report.finalize_store();
        tracing::info!(
            "Stored {} of {} chunk(s): files stored={} failed={} status={}",
            report.chunks_stored,
            chunks.len(),
            report.stored,
            report.failed,
            report.status()
        );
        Ok(report)
    }

    /// Store a single chunk (realm assignment included) as a private memory.
    async fn store_one_chunk(
        &self,
        chunk: &ingester::RawChunk,
        vector: &[f32],
        realm_hint: Option<&str>,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();

        // Assign to realm
        let realm_id = if let Some(hint) = realm_hint {
            self.store.find_realm_by_name(hint).await?.map(|r| r.id)
        } else {
            None
        };

        let realm_id = match realm_id {
            Some(rid) => rid,
            None => self.auto_assign_realm(vector).await?,
        };

        let realm = self.store.get_realm(&realm_id).await?;
        let realm_name = realm.map(|r| r.name.clone()).unwrap_or_default();

        // Infer reaction score from content
        let reaction_score = crate::engine::reactions::infer_reaction(&chunk.content);

        self.store
            .store_memory(
                &id,
                vector,
                &chunk.content,
                chunk.heading.as_deref(),
                Some(chunk.source_file.as_str()),
                &realm_id,
                &realm_name,
                &chunk.source_hash,
                &chunk.chunk_type,
                "general",
                reaction_score,
                // File ingestion is operator-driven; mark private
                // by default. Use PATCH /api/v1/memories/{id} to
                // flip a batch to public after ingestion.
                "private",
            )
            .await?;
        Ok(id)
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
        let outcome = ingester
            .ingest_path(&path.to_string_lossy(), chunk_by)
            .await?;

        let report = self
            .persist_ingested_chunks(outcome.chunks, realm_hint, outcome.report)
            .await?;

        tracing::info!(
            "  ✅ Re-ingested {} chunk(s) from {} (status={})",
            report.chunks_stored,
            path.display(),
            report.status()
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
        let query_vector = self.make_embedder()?.embed(query).await?;
        self.search_embedded(
            query,
            realm,
            tags,
            limit,
            offset,
            min_score,
            None,
            &query_vector,
        )
        .await
    }

    async fn search_embedded(
        &self,
        query: &str,
        realm: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
        min_score: f32,
        visibility: Option<&str>,
        query_vector: &[f32],
    ) -> anyhow::Result<Vec<MemoryResult>> {
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        let index = self.index.read().await;
        let hits = if tags.is_none_or(|t| t.is_empty()) {
            index.search(query_vector, (limit + offset) * 4)
        } else {
            None
        };
        let results = if let Some(hits) = hits {
            let mut fetched = Vec::new();
            for (id, score) in hits {
                if let Some(point) = self.store.get_memory(&id).await? {
                    if score >= min_score
                        && realm.is_none_or(|r| point.realm_name == r)
                        && visibility.is_none_or(|v| {
                            point.visibility == crate::storage::qdrant::normalize_visibility(v)
                        })
                    {
                        fetched.push((score, point));
                    }
                }
            }
            fetched
        } else {
            self.store
                .search_filtered(
                    query_vector,
                    limit + offset,
                    min_score,
                    realm,
                    tags,
                    visibility,
                )
                .await?
                .into_iter()
                .map(|r| (r.score, r.payload))
                .collect()
        };
        drop(index);

        // Hybrid: blend BM25 keyword matches in (see `lexical`). Tag-filtered
        // searches stay dense-only. Any failure falls back to the dense results.
        let hybrid = &self.config.search.hybrid;
        let results = if hybrid.enabled && tags.is_none_or(|t| t.is_empty()) {
            let hits = self
                .lexical
                .read()
                .await
                .search(query, hybrid.lexical_candidates);
            let have: std::collections::HashSet<&str> =
                results.iter().map(|(_, p)| p.id.as_str()).collect();
            let missing: Vec<String> = hits
                .iter()
                .filter(|(id, _)| !have.contains(id.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            let lexical_only = if missing.is_empty() {
                Ok(Vec::new())
            } else {
                self.store
                    .score_ids(query_vector, &missing, realm, visibility)
                    .await
                    .map(|v| v.into_iter().map(|r| (r.score, r.payload)).collect())
            };
            match lexical_only {
                Ok(lexical_only) => lexical::apply_lexical_bonus(
                    results,
                    lexical_only,
                    &hits,
                    hybrid.lexical_weight,
                    min_score,
                ),
                Err(e) => {
                    tracing::warn!("Keyword candidates skipped: {e}");
                    lexical::apply_lexical_bonus(
                        results,
                        Vec::new(),
                        &hits,
                        hybrid.lexical_weight,
                        min_score,
                    )
                }
            }
        } else {
            results
        };

        // Person lookup ("who is X"): long-lived memories naming X join the
        // candidates even below `min_score` -- the name match is the evidence --
        // and are ranked ahead of everything else below. Tag-filtered searches
        // and every other query shape are unaffected.
        let person_terms = if tags.is_none_or(|t| t.is_empty()) {
            person_lookup::person_terms(query)
        } else {
            Vec::new()
        };
        let mut person_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let results = if person_terms.is_empty() {
            results
        } else {
            let mut results = results;
            match self
                .store
                .score_by_types(
                    query_vector,
                    person_lookup::LONG_LIVED_TYPES,
                    person_lookup::CANDIDATES,
                    realm,
                    visibility,
                )
                .await
            {
                Ok(found) => {
                    for r in found {
                        if !person_lookup::names_all(&r.payload.content, &person_terms) {
                            continue;
                        }
                        person_ids.insert(r.payload.id.clone());
                        if !results.iter().any(|(_, p)| p.id == r.payload.id) {
                            results.push((r.score, r.payload));
                        }
                    }
                }
                Err(e) => tracing::warn!("Person lookup skipped: {e}"),
            }
            results
        };

        // Kept-and-remembered dreams are real memories of type `dream`; keep them out of
        // every search unless the caller explicitly filters on the `dream` tag.
        let results: Vec<_> = dream::exclude_dream_memories(results, tags)
            .into_iter()
            .filter(|(_, p)| !erased.contains(&p.id))
            .collect();

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

        // Query intent re-weighting: adjust scores based on inferred query intent.
        // Maps Mnemosyne's (vector, FTS, importance) biases to memex8's
        // (vector, importance, recency) since memex8 has no FTS layer.
        let intent_weights = query_intent::weights_for(query);
        results = results
            .into_iter()
            .map(|(score, p)| {
                // Compute per-memory recency boost (same Weibull formula as recall)
                let mem_type = if p.memory_type.is_empty() {
                    "general"
                } else {
                    &p.memory_type
                };
                let query_time = chrono::Utc::now();
                let recency = if p.is_certified() {
                    1.0
                } else {
                    decay::weibull_boost(&p.last_accessed, query_time, mem_type) as f32
                };

                // Vector score already includes similarity; we apply bias to it.
                // importance: pull from payload.
                // recency: recompute and bias.
                // upvote_factor: independent of intent — explicit user signal always wins.
                let upvote_factor = 1.0 + (p.upvotes as f32) * 0.18;
                let adjusted = score
                    * intent_weights.vector_bias
                    * (1.0 + (p.importance - 0.5) * (intent_weights.importance_bias - 1.0))
                    * (1.0 + (recency - 0.5) * (intent_weights.recency_bias - 1.0))
                    * upvote_factor;
                (adjusted, p)
            })
            .collect();
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        if !person_ids.is_empty() {
            // Stable: each group keeps its score order.
            results.sort_by_key(|(_, p)| !person_ids.contains(&p.id));
        }

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
                summary: p.summary,
                ingested_at: p.ingested_at,
                memory_type: p.memory_type,
                verification_status: p.verification_status,
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
        let _read = self.store.maintenance_read_guard().await;
        anyhow::ensure!(!self.erased_ids()?.contains(id), "Memory not found: {}", id);
        self.store
            .get_memory(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Memory not found: {}", id))
    }

    /// Fail if `id` has a pending or completed erasure intent.
    ///
    /// Erasure intents are permanent read barriers even before the projection
    /// deletion finishes. Transport handlers call this after fetching content
    /// and before publishing it, so a tombstoned memory can be neither echoed
    /// nor upvoted even if the transport gate is bypassed. Takes the shared
    /// maintenance guard itself, so it is safe standalone and nested inside a
    /// transport read scope.
    pub async fn refuse_erased(&self, id: &str) -> anyhow::Result<()> {
        let _read = self.store.maintenance_read_guard().await;
        anyhow::ensure!(!self.erased_ids()?.contains(id), "Memory not found: {}", id);
        Ok(())
    }

    pub async fn recall(
        &self,
        limit: usize,
        realm: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        // Scroll all memories and sort by importance × recency score
        let memories = self.store.scroll_all_memories().await?;

        let now = chrono::Utc::now().timestamp() as f64;
        let mut scored: Vec<_> = memories
            .into_iter()
            .filter(|m| !erased.contains(&m.id))
            .filter(|m| realm.as_deref().map_or(true, |r| m.realm_name == r))
            .filter(|m| m.memory_type != dream::DREAM_TYPE)
            .map(|m| {
                // Per-type Weibull decay (replaces uniform 1/(1 + days * 0.1))
                // Empty/legacy memory_type falls back to "general" defaults.
                let mem_type = if m.memory_type.is_empty() {
                    "general"
                } else {
                    &m.memory_type
                };
                let query_time = chrono::DateTime::from_timestamp(now as i64, 0)
                    .unwrap_or_else(|| chrono::Utc::now());
                let recency = if m.is_certified() {
                    1.0
                } else {
                    decay::weibull_boost(&m.last_accessed, query_time, mem_type) as f32
                };
                // Upvote factor: each upvote adds a multiplicative boost that saturates
                // gently via log1p. 1 upvote ≈ 1.18×, 5 upvotes ≈ 1.79×, 20 ≈ 3.05×.
                // Independent of importance so users can flag signal even on under-scored entries.
                let upvote_factor = 1.0 + (m.upvotes as f32) * 0.18;
                // Phase A.2: soft verification down-weight (never filters)
                let score = m.importance
                    * recency
                    * (1.0 + m.access_count as f32 * 0.05)
                    * upvote_factor
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
                summary: m.summary,
                ingested_at: m.ingested_at,
                memory_type: m.memory_type,
                verification_status: m.verification_status,
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

    /// List all memories with optional realm/visibility filter and sort, without recency weighting.
    pub async fn list_memories(
        &self,
        realm: Option<&str>,
        sort: &str,
        descending: bool,
        visibility: Option<&str>,
    ) -> anyhow::Result<Vec<crate::storage::qdrant::MemoryPoint>> {
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        // Visibility filter applied server-side when set (faster than
        // post-scroll in-memory filtering on large stores).
        let all = match visibility {
            Some(v) => self.store.scroll_memories_by_visibility(v).await?,
            None => self.store.scroll_all_memories().await?,
        };

        // Filter by realm if specified
        let filtered: Vec<_> = match realm {
            Some(r) => all.into_iter().filter(|m| &m.realm_name == r).collect(),
            None => all,
        };

        let mut sorted: Vec<_> = filtered
            .into_iter()
            .filter(|m| !erased.contains(&m.id))
            .collect();
        sort_memories(&mut sorted, sort, descending);
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
        self.refuse_evidence_projection_transfer().await?;
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
        self.refuse_evidence_projection_transfer().await?;
        // TODO: k-means split with auto-generated names
        tracing::info!("Splitting realm: {}", name);
        Ok(())
    }

    /// Bump a memory's upvote count and importance.
    ///
    /// Deliberately takes no exclusive maintenance guard: REST
    /// `upvote-by-content` runs under the retrieval read gate and calls this, so
    /// acquiring the write side here would deadlock that handler behind its own
    /// read scope. It still fails closed for erased IDs via `get_memory`.
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

    /// Partial payload update — used by PATCH /api/v1/memories/{id}.
    /// Accepts a JSON object whose keys are merged into the memory's Qdrant payload.
    pub async fn update_memory_payload(
        &self,
        id: &str,
        payload: qdrant_client::Payload,
    ) -> anyhow::Result<()> {
        self.refuse_evidence_mutation(id).await?;
        let _guard = self.store.maintenance_guard().await;
        self.store.update_memory_payload(id, payload).await
    }

    pub async fn prune_queue(&self) -> anyhow::Result<Vec<crate::storage::qdrant::MemoryPoint>> {
        let memories = self.store.scroll_all_memories().await?;
        let now = chrono::Utc::now();
        let default_auto_archive_days = self.config.slumber.auto_archive_days as i64;
        let prune_threshold = self.config.slumber.prune_threshold;

        Ok(memories
            .into_iter()
            .filter(|m| {
                // Low importance AND (old OR no access)
                if m.is_certified() || m.importance > prune_threshold {
                    return false;
                }
                let mem_type_for_age = if m.memory_type.is_empty() {
                    "general"
                } else {
                    m.memory_type.as_str()
                };
                let auto_archive_days =
                    self.config
                        .slumber
                        .auto_archive_days_by_type
                        .get(mem_type_for_age)
                        .copied()
                        .unwrap_or(default_auto_archive_days as u32) as i64;
                let age_ok = chrono::DateTime::parse_from_rfc3339(&m.ingested_at)
                    .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() > auto_archive_days)
                    .unwrap_or(false);
                let no_access = m.access_count == 0;
                age_ok || no_access
            })
            .collect())
    }

    /// Archive a memory by dropping its importance so slumber's prune phase
    /// reclaims it.
    ///
    /// Certified memories never decay, and archiving is decay by another name:
    /// it is refused (409) so an operator has to revoke certification first.
    /// The write itself is the certified-excluding filtered update used by every
    /// other automatic mutation, under the maintenance gate — a certification
    /// that lands concurrently wins the race and this call reports the conflict
    /// instead of decaying a certified memory. Explicit deletion is a separate
    /// path and stays allowed for certified memories.
    pub async fn archive_memory(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self.store.maintenance_guard().await;
        let Some(memory) = self.store.get_memory(id).await? else {
            return Err(crate::storage::qdrant::RecoveryError::NotFound(format!(
                "Memory not found: {}",
                id
            ))
            .into());
        };
        if memory.is_certified() {
            return Err(crate::storage::qdrant::RecoveryError::Conflict(
                "cannot archive a certified memory; revoke certification first".into(),
            )
            .into());
        }
        self.store
            .update_unprotected_payload(
                id,
                serde_json::json!({"upvotes": 0, "importance": 0.01}).try_into()?,
            )
            .await?;
        // Read back: if another writer certified the memory between the check
        // and the filtered write, the update was a no-op and the caller must
        // not be told the memory was archived.
        if self
            .store
            .get_memory(id)
            .await?
            .is_some_and(|memory| memory.is_certified())
        {
            return Err(crate::storage::qdrant::RecoveryError::Conflict(
                "cannot archive a certified memory; it was certified after the check".into(),
            )
            .into());
        }
        Ok(())
    }

    pub async fn delete_memory(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            uuid::Uuid::parse_str(id).is_ok(),
            "expected UUID memory identifier"
        );
        let _guard = self.store.maintenance_guard().await;
        // Graph edges and association lists are projections of the live corpus,
        // and nothing re-derives them for a single deletion. Drop every reference
        // to this memory *before* the delete: afterwards a success would leave
        // `graph/traverse` paths and `related_memory_ids` pointing at an ID that
        // no longer resolves. Pruning is idempotent, so a delete that fails after
        // this point just means the next run prunes again.
        self.store.prune_duplicate_references(id).await?;
        if let Some(ledger) = self.evidence_store()? {
            if ledger.begin_erase(id)?.is_some() {
                // The compressed index has no per-ID removal; never keep a stale
                // candidate map that still resolves this ID.
                self.index
                    .around_write(self.store.delete_projection_after_erasure_intent(id))
                    .await?;
                ledger.finish_erase(id)?;
                self.lexical.write().await.remove(id);
                return Ok(());
            }
        }
        self.index
            .around_write(self.store.delete_memory(id))
            .await?;
        self.lexical.write().await.remove(id);
        Ok(())
    }

    pub async fn edit_memory(&self, id: &str, new_content: &str) -> anyhow::Result<()> {
        let clean_content = redaction::required(new_content)?;
        let new_content = clean_content.as_str();
        let _guard = self.store.maintenance_guard().await;
        self.refuse_evidence_mutation(id).await?;
        let mut existing = self
            .store
            .get_memory(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Memory not found: {}", id))?;
        if existing.is_certified() {
            return Err(crate::storage::qdrant::CertifiedMemoryError.into());
        }
        existing = redaction::sanitize(&existing)?;
        let embedder = self.make_embedder()?;
        let vector = embedder.embed(new_content).await?;
        existing.content = new_content.to_string();
        existing.summary = None;
        existing.verification_status = "unverified".into();
        existing.last_verified = None;
        existing.verification_confidence = None;
        existing.reaction_score = crate::engine::reactions::infer_reaction(new_content);
        // The stored vector changes, so the compressed index no longer describes
        // this memory; search serves from Qdrant until the next rebuild.
        self.index
            .around_write(self.store.replace_memory(&existing, &vector))
            .await?;
        self.lexical
            .write()
            .await
            .upsert(id, &lexical::memory_text(&existing));
        tracing::info!("Updated memory {}", id);
        Ok(())
    }

    pub async fn slumber_status(&self) -> SlumberStatus {
        let paused = self.pause.refresh();
        let state = self.slumber_state.read().await;
        SlumberStatus {
            state: if paused {
                "paused".into()
            } else {
                state.status.clone()
            },
            paused,
            pause_file: self.pause.path().to_string(),
            last_run: state.last_run.map(|t| t.to_rfc3339()),
            next_scheduled: None,
            memories_processed: state.memories_processed,
            realms_reorganized: state.realms_reorganized,
            last_report: state.last_report.clone(),
        }
    }

    /// The operator pause flag for scheduled and explicit slumber runs.
    pub fn slumber_pause(&self) -> &slumber_pause::SlumberPause {
        &self.pause
    }

    /// Rebuild every derived search structure from the live corpus. Called after
    /// multi-memory maintenance (slumber, import, restore) whose last mutation
    /// is already durable.
    pub(crate) async fn rebuild_derived_indexes(&self) {
        self.index.rebuild_and_reload().await;
        if let Err(e) = self.rebuild_lexical_index().await {
            tracing::warn!("Lexical index rebuild failed: {e}");
        }
    }

    pub async fn trigger_slumber(
        &self,
        force_consolidation: bool,
    ) -> anyhow::Result<slumber::SlumberReport> {
        self.refuse_evidence_transfer()?;
        // Pause is authoritative for explicit triggers too, and it is re-read
        // here so a pause written by another process is honoured immediately.
        if self.pause.refresh() {
            tracing::info!("⏸️  Slumber trigger refused: paused");
            return Err(SlumberPausedError.into());
        }
        {
            let mut state = self.slumber_state.write().await;
            state.status = "running".into();
        }

        tracing::info!("💤 Slumber started...");
        let slumber = slumber::SlumberEngine::new(self.config.clone(), self.store.clone_store());
        // Slumber mutates the corpus outside the per-write hooks and rebuilds
        // the compressed index at the end of its own pipeline. Drop the
        // in-memory index first so searches during the run read Qdrant instead
        // of a map the run is about to replace.
        self.index.invalidate().await;
        // Run the pipeline on its own task. A panic then surfaces as a
        // JoinError instead of unwinding the caller (the scheduler loop) and
        // leaving the status stuck at "running" with no way back to idle. The
        // maintenance gate inside the pipeline remains the only serializer
        // between runs.
        let result =
            match tokio::spawn(async move { slumber.run_full_pipeline(force_consolidation).await })
                .await
            {
                Ok(result) => result,
                Err(e) if e.is_panic() => Err(anyhow::anyhow!(
                    "slumber pipeline panicked: {}",
                    panic_message(e)
                )),
                Err(e) => Err(anyhow::anyhow!("slumber pipeline task failed: {}", e)),
            };
        // Slumber rewrites, merges and deletes memories without going through the
        // engine hooks, so refresh the keyword index from Qdrant afterwards. The
        // compressed index was rebuilt by the pipeline's last phase; install it
        // now (after re-verifying it) instead of waiting for a restart.
        if let Err(e) = self.rebuild_lexical_index().await {
            tracing::warn!("Lexical index rebuild after slumber failed: {e}");
        }
        if result.is_ok() {
            self.index.reload().await;
        }
        // Reset even after an archive/index/storage failure or a panic so status
        // is accurate and the next run is not blocked by a stale "running".
        if result.is_err() {
            self.slumber_state.write().await.status = "idle".into();
        }
        let report = result?;

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

    pub async fn graph_search(
        &self,
        entity: &str,
        _relationship: Option<&str>,
        _depth: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        // TODO: implement knowledge graph traversal
        // For now, fall back to semantic search and wrap results
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        let embedder = self.make_embedder()?;
        let query_vector = embedder.embed(entity).await?;
        let results = self.store.search(&query_vector, 10, 0.3, None).await?;

        let output: Vec<serde_json::Value> = results
            .iter()
            .filter(|r| !erased.contains(&r.payload.id))
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
        let _guard = self.store.maintenance_guard().await;
        self.refuse_evidence_projection_transfer().await?;
        // Export with vectors so they can be reused on import
        let memories = self.store.scroll_all_memories_with_vectors().await?;
        let json = serde_json::to_string_pretty(&memories)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub async fn import(&self, path: &str, reuse_vectors: bool) -> anyhow::Result<usize> {
        self.refuse_evidence_projection_transfer().await?;
        let _guard = self.store.maintenance_guard().await;
        let content = std::fs::read_to_string(path)?;
        let memories: Vec<MemoryWithVector> = if reuse_vectors {
            serde_json::from_str(&content)
                .or_else(|_| {
                    serde_json::from_str::<Vec<crate::storage::qdrant::MemoryPoint>>(&content).map(
                        |points| {
                            points
                                .into_iter()
                                .map(|memory| MemoryWithVector {
                                    memory,
                                    vector: Vec::new(),
                                })
                                .collect()
                        },
                    )
                })
                .map_err(|_| anyhow::anyhow!("Invalid memory import format"))?
        } else {
            serde_json::from_str::<Vec<crate::storage::qdrant::MemoryPoint>>(&content)
                .map_err(|_| anyhow::anyhow!("Invalid memory import format"))?
                .into_iter()
                .map(|memory| MemoryWithVector {
                    memory,
                    vector: Vec::new(),
                })
                .collect()
        };
        anyhow::ensure!(
            memories.iter().all(|m| m.memory.evidence_refs.is_empty()),
            "evidence references in import require paired SQLite restore"
        );
        let mut stored = 0;
        for snapshot in memories {
            let mut memory: crate::storage::qdrant::MemoryPoint =
                redaction::sanitize(&snapshot.memory)?;
            if !redaction::meaningful(&memory.content) {
                tracing::info!("Skipped imported memory: content empty after redaction");
                continue;
            }
            let changed = memory.content != snapshot.memory.content;
            if changed {
                memory.source_hash = crate::storage::qdrant::content_hash(&memory.content);
            }
            let vector = if reuse_vectors && !changed && !snapshot.vector.is_empty() {
                snapshot.vector
            } else {
                self.make_embedder()?.embed(&memory.content).await?
            };
            self.store.replace_memory(&memory, &vector).await?;
            stored += 1;
        }
        // Import replaces or adds memories behind the per-write hooks, so both
        // derived caches are rebuilt from the final corpus.
        self.rebuild_derived_indexes().await;
        Ok(stored)
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

    /// The Qdrant store, for the modules inside `engine`. Code outside the
    /// engine goes through Engine methods, [`Engine::gate`] for the maintenance
    /// gate and [`Engine::set_certification`] for certification, so a change to
    /// how memories are stored stays inside this module.
    pub(in crate::engine) fn store(&self) -> &QdrantStore {
        &self.store
    }

    /// The maintenance gate. Read-only transports hold its read side through
    /// response construction so an erasure cannot commit between fetching a
    /// memory and building the response (see `mcp::publish`).
    pub fn gate(&self) -> &crate::storage::gate::MaintenanceGate {
        self.store.gate()
    }

    /// Certify (or revoke certification of) a memory. `None` when it does not exist.
    pub async fn set_certification(
        &self,
        id: &str,
        revoke: bool,
        by: Option<&str>,
        note: Option<&str>,
    ) -> anyhow::Result<Option<crate::storage::qdrant::MemoryPoint>> {
        self.store.set_certification(id, revoke, by, note).await
    }

    /// Test-only access to the store, for asserting persisted state. Not for
    /// production code: `tests/engine_seam.rs` fails if `src/` calls it.
    #[doc(hidden)]
    pub fn store_for_tests(&self) -> &QdrantStore {
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
            let _results = self.search(t, None, None, 5, 0, 0.3).await?;
            // Collect all memories and their topic clusters
            let all_memories = self.store.scroll_all_memories().await?;
            let mut cluster_memories: std::collections::HashMap<
                String,
                Vec<&crate::storage::qdrant::MemoryPoint>,
            > = std::collections::HashMap::new();
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
        suggestions.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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
        // Session embedding must use the active provider/dimensions, including env overrides.
        let mut config = self.config.clone();
        config.embedding.provider = self.embed_provider.clone();
        config.embedding.model = self.embed_model.clone();
        config.embedding.dimensions = self.embed_dimensions;
        config.embedding.openai.base_url = self.openai_base_url.clone();
        config.embedding.openai.dimensions = self.embed_dimensions;
        let session_engine = crate::engine::session::SessionEngine::new(config, self.store.clone());
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
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        if erased.contains(memory_id) {
            return Ok(vec![]);
        }
        let graph = graph::KnowledgeGraph::new(self.store.clone());
        Ok(graph
            .search_graph(memory_id, depth)
            .await?
            .into_iter()
            .filter(|r| {
                !erased.contains(&r.memory_id)
                    && r.path
                        .iter()
                        .all(|s| !erased.contains(&s.from_id) && !erased.contains(&s.to_id))
            })
            .collect())
    }

    /// Get directly connected memories for a given memory.
    pub async fn graph_neighbors(
        &self,
        memory_id: &str,
    ) -> anyhow::Result<Vec<graph::Relationship>> {
        let _read = self.store.maintenance_read_guard().await;
        let erased = self.erased_ids()?;
        if erased.contains(memory_id) {
            return Ok(vec![]);
        }
        let graph = graph::KnowledgeGraph::new(self.store.clone());
        Ok(graph
            .get_neighbors(memory_id)
            .await?
            .into_iter()
            .filter(|r| !erased.contains(&r.from_id) && !erased.contains(&r.to_id))
            .collect())
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
    pub async fn restore(
        &self,
        backup_path: &str,
        force: bool,
        acknowledge_legacy_evidence_resurrection: bool,
    ) -> anyhow::Result<usize> {
        let n = backup::restore(
            self,
            backup_path,
            force,
            acknowledge_legacy_evidence_resurrection,
        )
        .await?;
        // A restore replaces the corpus wholesale, so both derived caches are
        // rebuilt from the restored state.
        self.rebuild_derived_indexes().await;
        Ok(n)
    }

    /// List available backups sorted by date (newest first).
    pub fn list_backups(
        &self,
        backup_dir: Option<&str>,
    ) -> anyhow::Result<Vec<backup::BackupInfo>> {
        backup::list_backups(backup_dir)
    }
}

/// Text of a panicked slumber task, for the error the caller sees.
fn panic_message(error: tokio::task::JoinError) -> String {
    let payload = error.into_panic();
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Async locks keyed by content hash. Entries are weak, so a key's lock is
/// freed as soon as no write holds it; dead entries are swept on each lookup.
#[derive(Default)]
struct IdempotencyLocks(
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
);

impl IdempotencyLocks {
    fn get(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = map.get(key).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        map.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        map.insert(key.to_string(), Arc::downgrade(&lock));
        lock
    }
}

/// Order memories for `GET /api/v1/memories`. `descending` puts the largest
/// value first (highest importance, newest timestamp, most accesses); unknown
/// sort keys fall back to `ingested_at`. NaN importance sorts without panicking.
pub(crate) fn sort_memories(
    memories: &mut [crate::storage::qdrant::MemoryPoint],
    sort: &str,
    descending: bool,
) {
    use std::cmp::Ordering;
    let key: fn(
        &crate::storage::qdrant::MemoryPoint,
        &crate::storage::qdrant::MemoryPoint,
    ) -> Ordering = match sort {
        "importance" => |a, b| a.importance.total_cmp(&b.importance),
        "last_accessed" => |a, b| a.last_accessed.cmp(&b.last_accessed),
        "access_count" => |a, b| a.access_count.cmp(&b.access_count),
        _ => |a, b| a.ingested_at.cmp(&b.ingested_at),
    };
    if descending {
        memories.sort_by(|a, b| key(b, a));
    } else {
        memories.sort_by(key);
    }
}

#[cfg(test)]
mod tests {
    use super::verification_score_multiplier;
    #[test]
    fn test_verification_score_multiplier_values() {
        assert_eq!(verification_score_multiplier("certified"), 1.05);
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

    fn listed(
        id: &str,
        importance: f32,
        ingested_at: &str,
        access_count: u32,
    ) -> crate::storage::qdrant::MemoryPoint {
        serde_json::from_value(serde_json::json!({
            "id": id, "content": id, "summary": null, "source_file": null,
            "realm_id": null, "realm_name": "r", "importance": importance, "upvotes": 0,
            "tags": [], "ingested_at": ingested_at, "last_accessed": ingested_at,
            "access_count": access_count, "chunk_type": "manual", "heading": null, "source_hash": ""
        }))
        .unwrap()
    }

    fn ids(memories: &[crate::storage::qdrant::MemoryPoint]) -> Vec<&str> {
        memories.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn test_sort_memories_descending_puts_largest_first() {
        // Regression: `direction=desc` (the API default, documented as
        // "newest first") returned the oldest / least important memories.
        let mut m = vec![
            listed("old", 0.2, "2026-05-01T00:00:00Z", 5),
            listed("new", 0.9, "2026-11-12T00:00:00Z", 1),
            listed("mid", 0.5, "2026-08-01T00:00:00Z", 9),
        ];
        super::sort_memories(&mut m, "ingested_at", true);
        assert_eq!(ids(&m), ["new", "mid", "old"]);
        super::sort_memories(&mut m, "importance", true);
        assert_eq!(ids(&m), ["new", "mid", "old"]);
        super::sort_memories(&mut m, "access_count", true);
        assert_eq!(ids(&m), ["mid", "old", "new"]);
        super::sort_memories(&mut m, "unknown-key", true);
        assert_eq!(ids(&m), ["new", "mid", "old"]);
    }

    #[test]
    fn test_sort_memories_ascending_puts_smallest_first() {
        let mut m = vec![
            listed("new", 0.9, "2026-11-12T00:00:00Z", 1),
            listed("old", 0.2, "2026-05-01T00:00:00Z", 5),
            listed("mid", 0.5, "2026-08-01T00:00:00Z", 9),
        ];
        super::sort_memories(&mut m, "ingested_at", false);
        assert_eq!(ids(&m), ["old", "mid", "new"]);
        super::sort_memories(&mut m, "importance", false);
        assert_eq!(ids(&m), ["old", "mid", "new"]);
    }

    #[test]
    fn test_sort_memories_nan_importance_does_not_panic() {
        let mut m = vec![
            listed("a", 0.0, "2026-01-01T00:00:00Z", 0),
            listed("b", 0.5, "2026-01-02T00:00:00Z", 0),
        ];
        // JSON has no NaN, so set it after deserialising the fixture.
        m[0].importance = f32::NAN;
        super::sort_memories(&mut m, "importance", true);
        assert_eq!(m.len(), 2);
    }
}
