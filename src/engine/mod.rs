pub mod associations;
pub mod backup;
pub mod chunker;
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
pub mod providers;
pub mod quantizer;
pub mod query_intent;
pub mod reactions;
pub mod realms;
pub mod redaction;
pub mod retrieval;
pub mod scheduler;
pub mod session;
pub mod slumber;
pub mod timeline;
pub mod watcher;

use crate::config::AppConfig;
pub use crate::engine::watcher::{FileChangeEvent, FileWatcher};
use crate::storage::qdrant::{MemoryWithVector, QdrantStore};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Whether a persisted compressed index may be trusted for the live corpus.
///
/// Counting is not enough. TurboVec is preferred over Qdrant in the search path,
/// so a stale index does not merely return old results — it returns *fewer*
/// results. Slumber's consolidation deletes source memories and inserts
/// summaries, which keeps the vector count roughly stable while changing every
/// ID in the affected region: a count-only check accepted exactly that index and
/// left the summaries unsearchable after a restart. Compare the ID sets instead.
/// Any disagreement rejects the index and search falls back to Qdrant, which is
/// always authoritative.
pub fn turbovec_index_matches_corpus(indexed: &[String], live: &HashSet<String>) -> bool {
    indexed.len() == live.len() && indexed.iter().all(|id| live.contains(id))
}

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
    /// TurboVec compressed vector index for fast search.
    /// Loaded from disk on startup, rebuilt at the end of every slumber run.
    /// Cleared by `invalidate_compressed_index` on every corpus mutation.
    ///
    /// There is no in-process freshness counter here: every mutation already
    /// drops this handle, so nothing can observe a lagging index in memory. The
    /// freshness guarantee lives on disk instead — the persisted pair carries a
    /// corpus fingerprint and file digests (`CompressedIndexMeta`) that are
    /// re-checked before an index is ever installed (see
    /// `init_turbovec_index`). An index that describes a different corpus is
    /// rejected there, so search falls back to Qdrant, which is complete.
    turbovec_index: Arc<RwLock<Option<quantizer::TurboQuantVectorIndex>>>,
    /// BM25 keyword index (a cache rebuilt from Qdrant; see `lexical`).
    lexical: Arc<RwLock<lexical::LexicalIndex>>,
    /// Dreams live in their own files, outside the memories collection.
    dreams: dream::DreamStore,
    dream_lock: Arc<tokio::sync::Mutex<()>>,
    /// Operator pause for scheduled and explicit slumber runs. Mirrored in
    /// memory for status reads but re-read from [`slumber_pause_path`] on every
    /// scheduler tick and trigger, so a CLI process sharing the data volume can
    /// stop the daemon's maintenance without an RPC.
    slumber_pause: Arc<AtomicBool>,
    slumber_pause_path: String,
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
        let turbovec_index = Self::init_turbovec_index(&config, &store, dimensions as usize).await;

        // Dreams sit next to the TurboVec index, so they're on the data volume and in backups.
        let dreams = dream::DreamStore::new(
            std::path::Path::new(&config.turbovec.index_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("dreams"),
        );
        let slumber_pause_path = slumber_pause_path(&config);
        let slumber_pause = Arc::new(AtomicBool::new(read_slumber_pause(&slumber_pause_path)));
        if slumber_pause.load(Ordering::SeqCst) {
            tracing::warn!(
                "⏸️  Slumber is paused in {}; scheduled runs are refused until resumed",
                slumber_pause_path
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
            turbovec_index,
            lexical: Arc::new(RwLock::new(lexical::LexicalIndex::default())),
            dreams,
            dream_lock: Arc::new(tokio::sync::Mutex::new(())),
            slumber_pause,
            slumber_pause_path,
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
            .store_memory(
                &dream::memory_text_for(&d),
                Some(vec![dream::DREAM_TYPE.to_string()]),
                None,
                Some("dream"),
                None,
                Some(dream::DREAM_TYPE),
            )
            .await?;
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
    async fn init_turbovec_index(
        config: &AppConfig,
        store: &QdrantStore,
        dims: usize,
    ) -> Arc<RwLock<Option<quantizer::TurboQuantVectorIndex>>> {
        let (index_path, id_map_path) = compressed_index_paths(config);
        let bit_width = config.turbovec.bit_width;
        let nothing = Arc::new(RwLock::new(None));

        if !std::path::Path::new(&index_path).exists()
            || !std::path::Path::new(&id_map_path).exists()
        {
            tracing::debug!(
                "TurboVec index not found at {} (will create on first slumber)",
                index_path
            );
            return nothing;
        }
        // A failed unlink leaves files behind; the marker written at that moment
        // is what keeps them from being trusted.
        let marker = compressed_index_stale_marker(&index_path);
        if std::path::Path::new(&marker).exists() {
            tracing::warn!(
                "⚠️ Compressed index pair at {} was marked stale by an invalidation that \
                 could not remove it ({}); ignoring it and serving search from Qdrant \
                 until the next slumber rebuilds it",
                index_path,
                marker
            );
            return nothing;
        }
        let index = match quantizer::TurboQuantVectorIndex::load(
            &index_path,
            &id_map_path,
            dims,
            bit_width,
        ) {
            Ok(index) => index,
            Err(e) => {
                // NotFound is handled above, so anything here is a corrupt or
                // mixed pair: never silently treat it as "no index yet".
                tracing::warn!(
                    "⚠️ Compressed index at {} is unreadable or inconsistent ({e}); \
                     ignoring it and falling back to Qdrant until the next slumber rebuilds it",
                    index_path
                );
                return nothing;
            }
        };
        let meta = match read_compressed_index_meta(&index_path) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                tracing::warn!(
                    "⚠️ Compressed index at {} has no fingerprint sidecar, so it cannot be \
                     proven fresh; ignoring it and falling back to Qdrant",
                    index_path
                );
                return nothing;
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️ Compressed index sidecar for {} could not be read ({e}); \
                     ignoring the pair and falling back to Qdrant",
                    index_path
                );
                return nothing;
            }
        };
        if let Err(e) = meta.validate_pair(&index_path, &id_map_path, &index, dims) {
            tracing::warn!(
                "⚠️ Compressed index at {} does not match its own sidecar ({e}); \
                 ignoring it and falling back to Qdrant until the next slumber rebuilds it",
                index_path
            );
            return nothing;
        }
        // Without the live corpus the index cannot be proven fresh, and a stale
        // index hides memories instead of returning old ones.
        let live = match store.scroll_all_memories().await {
            Ok(memories) => memories,
            Err(e) => {
                tracing::warn!(
                    "⚠️ Could not read the live corpus to validate {} ({e}); \
                     ignoring it and serving search from Qdrant",
                    index_path
                );
                return nothing;
            }
        };
        if meta.fingerprint != corpus_fingerprint(&live) {
            tracing::warn!(
                "⚠️ TurboVec index at {} was built for a different corpus: memory content \
                 changed since the rebuild ({} live memories); ignoring it and falling back \
                 to Qdrant until the next slumber rebuilds it",
                index_path,
                live.len()
            );
            return nothing;
        }
        let live_ids: HashSet<String> = live.into_iter().map(|m| m.id).collect();
        if turbovec_index_matches_corpus(index.ids(), &live_ids) {
            tracing::info!(
                "📦 Loaded TurboVec index: {} vectors from {} ({}d @ {}-bit, {:.1}x compression)",
                index.vector_count(),
                index_path,
                dims,
                bit_width,
                index.compression_ratio()
            );
            Arc::new(RwLock::new(Some(index)))
        } else {
            let stale = index
                .ids()
                .iter()
                .filter(|id| !live_ids.contains(*id))
                .count();
            tracing::warn!(
                "⚠️ TurboVec index at {} holds {} vectors for {} live memories \
                 ({} indexed IDs are no longer live); ignoring it and falling back \
                 to Qdrant until the next slumber rebuilds it",
                index_path,
                index.vector_count(),
                live_ids.len(),
                stale
            );
            nothing
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
        // when it was built, so it would hide every chunk ingested here. Drop it
        // (and the files on disk) before the first write, like `store_memory`.
        self.invalidate_compressed_index().await;

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
                    tracing::warn!("Failed to store chunk from {}: {}", chunk.source_file, e);
                    report.note_chunk_failed(&chunk.source_file);
                }
            }
        }

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
        let tv_guard = self.turbovec_index.read().await;
        // The index handle is only ever installed by the load path after the
        // persisted pair was validated against this corpus, and dropped by every
        // mutation, so holding it here is enough: no epoch to compare.
        let results = if let Some(tv) = tv_guard
            .as_ref()
            .filter(|_| tags.is_none_or(|t| t.is_empty()))
        {
            let fetch_limit = (limit + offset) * 4;
            let (scores, indices) = tv.search(query_vector, fetch_limit);
            let ids = tv.resolve_ids(&indices);
            let mut fetched = Vec::new();
            for (id, score) in ids.iter().zip(scores) {
                if let Some(point) = self.store.get_memory(id).await? {
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
        drop(tv_guard);

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
                self.invalidate_compressed_index().await;
                self.store
                    .delete_projection_after_erasure_intent(id)
                    .await?;
                ledger.finish_erase(id)?;
                self.lexical.write().await.remove(id);
                return Ok(());
            }
        }
        self.invalidate_compressed_index().await;
        self.store.delete_memory(id).await?;
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
        // this memory. Drop it before the write and let search serve from Qdrant.
        self.invalidate_compressed_index().await;
        self.store.replace_memory(&existing, &vector).await?;
        self.lexical
            .write()
            .await
            .upsert(id, &lexical::memory_text(&existing));
        tracing::info!("Updated memory {}", id);
        Ok(())
    }

    pub async fn slumber_status(&self) -> SlumberStatus {
        let paused = self.refresh_slumber_pause().await;
        let state = self.slumber_state.read().await;
        SlumberStatus {
            state: if paused {
                "paused".into()
            } else {
                state.status.clone()
            },
            paused,
            pause_file: self.slumber_pause_path.clone(),
            last_run: state.last_run.map(|t| t.to_rfc3339()),
            next_scheduled: None,
            memories_processed: state.memories_processed,
            realms_reorganized: state.realms_reorganized,
            last_report: state.last_report.clone(),
        }
    }

    /// Re-read the persisted pause flag.
    ///
    /// Every scheduler tick and every trigger goes through here, so a pause
    /// written by another process (a CLI sharing the data volume, an operator's
    /// script) takes effect without restarting the daemon.
    pub async fn refresh_slumber_pause(&self) -> bool {
        let paused = read_slumber_pause(&self.slumber_pause_path);
        if paused != self.slumber_pause.load(Ordering::SeqCst) {
            tracing::info!(
                paused,
                path = %self.slumber_pause_path,
                "Slumber pause flag changed on disk"
            );
            self.slumber_pause.store(paused, Ordering::SeqCst);
        }
        paused
    }

    /// In-memory view of the pause flag (refreshed by the scheduler and by
    /// `slumber_status`).
    pub async fn slumber_paused(&self) -> bool {
        self.slumber_pause.load(Ordering::SeqCst)
    }

    pub fn slumber_pause_path(&self) -> &str {
        &self.slumber_pause_path
    }

    /// Persist the pause flag, then report success.
    ///
    /// The persisted write is what makes the pause real: it survives a restart
    /// and is visible to the scheduler of a *different* process on the same data
    /// volume. A write failure is returned, never swallowed, because reporting a
    /// pause the daemon would not honour is exactly the failure this replaces.
    pub async fn pause_slumber(&self) -> anyhow::Result<()> {
        write_slumber_pause(&self.slumber_pause_path, true)?;
        self.slumber_pause.store(true, Ordering::SeqCst);
        tracing::info!(
            "⏸️  Slumber paused ({}); scheduled runs are refused",
            self.slumber_pause_path
        );
        Ok(())
    }

    pub async fn resume_slumber(&self) -> anyhow::Result<()> {
        write_slumber_pause(&self.slumber_pause_path, false)?;
        self.slumber_pause.store(false, Ordering::SeqCst);
        tracing::info!("▶️  Slumber resumed");
        Ok(())
    }

    /// The single place that discards derived search structures after a corpus
    /// mutation. Qdrant stays authoritative while they are rebuilt.
    ///
    /// Lock order is `maintenance gate → compressed index → lexical`. Callers
    /// that already hold the gate call this before releasing it; search takes the
    /// same order (gate, then the index), so no cycle exists. In-memory state is
    /// always dropped; if the files on disk cannot be unlinked, a stale marker is
    /// written so the pair is rejected at the next load instead of being trusted.
    ///
    /// Every mutation that changes content, a vector, or which IDs are live must
    /// call this. Payload-only updates (importance, access counts, verification
    /// stamps) do not: they change neither the vector nor the ID membership.
    pub(crate) async fn invalidate_compressed_index(&self) {
        *self.turbovec_index.write().await = None;
        let (index_path, id_map_path) = compressed_index_paths(&self.config);
        if let Err(e) = dedup_removal::invalidate_index_files(&index_path, &id_map_path) {
            // A pair that survives a failed unlink must never be accepted again.
            // The marker is the load path's first check; if even that write fails
            // the pair is still rejected, because its stored fingerprint no longer
            // matches the corpus that just changed.
            let marker = compressed_index_stale_marker(&index_path);
            match std::fs::write(&marker, format!("invalidation failed: {e}\n")) {
                Ok(()) => tracing::warn!(
                    "Compressed index files could not be removed ({e}); wrote {marker} \
                     so the pair is rejected at startup"
                ),
                Err(marker_error) => tracing::error!(
                    "Compressed index files could not be removed ({e}) and the stale marker \
                     could not be written ({marker_error}); the leftover pair is still rejected \
                     at startup by its corpus fingerprint"
                ),
            }
        }
    }

    /// Rebuild every derived search structure from the live corpus. Called after
    /// multi-memory maintenance (slumber, import, restore) whose last mutation
    /// is already durable.
    pub(crate) async fn rebuild_derived_indexes(&self) {
        self.invalidate_compressed_index().await;
        if let Err(e) = rebuild_compressed_index(&self.store, &self.config).await {
            tracing::warn!("Compressed index rebuild failed: {e}");
        }
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
        if self.refresh_slumber_pause().await {
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
        self.invalidate_compressed_index().await;
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
        // compressed index was rebuilt by the pipeline's last phase; this engine
        // keeps serving from Qdrant until the next restart, as before.
        if let Err(e) = self.rebuild_lexical_index().await {
            tracing::warn!("Lexical index rebuild after slumber failed: {e}");
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

    pub async fn store_memory(
        &self,
        content: &str,
        tags: Option<Vec<String>>,
        realm_hint: Option<&str>,
        source: Option<&str>,
        visibility: Option<&str>,
        memory_type: Option<&str>,
    ) -> anyhow::Result<String> {
        self.store_memory_with_certification(
            content,
            tags,
            realm_hint,
            source,
            visibility,
            memory_type,
            false,
        )
        .await
    }

    pub async fn store_memory_with_certification(
        &self,
        content: &str,
        tags: Option<Vec<String>>,
        realm_hint: Option<&str>,
        source: Option<&str>,
        visibility: Option<&str>,
        memory_type: Option<&str>,
        certified: bool,
    ) -> anyhow::Result<String> {
        let clean_content = redaction::required(content)?;
        let content = clean_content.as_str();
        let tags = tags.map(|tags| {
            tags.into_iter()
                .map(|s| redaction::text(&s))
                .collect::<Vec<_>>()
        });
        let clean_realm = realm_hint.map(redaction::text);
        let realm_hint = clean_realm.as_deref();
        let clean_source = source.map(redaction::text);
        let source = clean_source.as_deref();
        let clean_type = memory_type.map(redaction::text);
        let memory_type = clean_type.as_deref();
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

        // Default to "private" — callers must explicitly opt in to sharing.
        let visibility_str =
            crate::storage::qdrant::normalize_visibility(visibility.unwrap_or("private"));

        // Caller-supplied memory_type wins; default to "general" only when
        // omitted. The 7-day Weibull decay for "general" is too aggressive
        // for personal-knowledge memories — callers should set type
        // explicitly (profile, preference, entity, etc) for 365-day
        // retention.
        let memory_type_str = memory_type.unwrap_or("general");

        // A new memory is not in any compressed index built earlier, and search
        // prefers that index. Drop it before the write so the new memory is
        // visible immediately instead of only after the next restart.
        self.invalidate_compressed_index().await;
        self.store
            .store_memory_with_tags(
                &id,
                &vector,
                content,
                None,
                Some(source_str),
                &realm_id,
                &realm_name,
                "",
                "manual",
                memory_type_str,
                reaction_score,
                visibility_str,
                tags.as_deref().unwrap_or(&[]),
                certified,
            )
            .await?;

        {
            let mut text = content.to_string();
            for t in tags.as_deref().unwrap_or(&[]) {
                text.push(' ');
                text.push_str(t);
            }
            self.lexical.write().await.upsert(&id, &text);
        }

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

/// Where the operator pause flag is persisted.
///
/// It sits beside the compressed index, i.e. on the data volume, so it survives
/// a daemon restart and is visible to any other process (the CLI) that resolves
/// the same config and environment.
pub fn slumber_pause_path(config: &AppConfig) -> String {
    let (index_path, _) = compressed_index_paths(config);
    std::path::Path::new(&index_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("slumber_paused.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("slumber_paused.json"))
        .to_string_lossy()
        .into_owned()
}

/// Read the persisted pause flag.
///
/// A missing, unreadable or malformed file means "not paused": the flag is an
/// operator convenience, so a truncated write must not silently disable
/// maintenance forever.
pub(crate) fn read_slumber_pause(path: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => value
                .get("paused")
                .and_then(|paused| paused.as_bool())
                .unwrap_or(false),
            Err(e) => {
                tracing::warn!("Ignoring malformed slumber pause file {path}: {e}");
                false
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            tracing::warn!("Slumber pause file {path} is unreadable: {e}");
            false
        }
    }
}

/// Durably write the pause flag (temporary sibling, then rename) before any
/// caller reports success.
pub(crate) fn write_slumber_pause(path: &str, paused: bool) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "paused": paused,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temp = format!("{path}.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&body)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
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

/// Configured compressed-index file pair, including the defaults used when the
/// config leaves the paths empty.
pub(crate) fn compressed_index_paths(config: &AppConfig) -> (String, String) {
    let index_path = if config.turbovec.index_path.is_empty() {
        "data/memories.tv".to_string()
    } else {
        config.turbovec.index_path.clone()
    };
    let id_map_path = if config.turbovec.id_map_path.is_empty() {
        "data/memories_ids.json".to_string()
    } else {
        config.turbovec.id_map_path.clone()
    };
    (index_path, id_map_path)
}

/// Sidecar that records which corpus a persisted index pair was built from.
///
/// The pair alone is not self-describing: the `.tv` holds quantized codes and the
/// id map holds slot→UUID, so stale *vectors* with unchanged IDs are invisible to
/// an ID-set check. The sidecar carries a fingerprint of the corpus content (not
/// `source_hash`, which `edit_memory` never updates) plus byte digests of both
/// files, so a load can prove the pair describes this corpus.
pub(crate) fn compressed_index_meta_path(index_path: &str) -> String {
    format!("{index_path}.meta.json")
}

/// Marker written when an invalidation could not unlink the pair. Its existence
/// alone rejects the pair at load; a successful rebuild removes it.
pub(crate) fn compressed_index_stale_marker(index_path: &str) -> String {
    format!("{index_path}.stale")
}

const COMPRESSED_INDEX_META_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CompressedIndexMeta {
    pub version: u32,
    /// Number of vectors in the `.tv` at the time it was written.
    pub vectors: usize,
    /// Number of IDs in the companion map; must equal `vectors`.
    pub id_map_len: usize,
    pub dimensions: usize,
    pub bit_width: usize,
    /// SHA-256 over the (id, content) pairs of the corpus this pair was built for.
    pub fingerprint: String,
    /// SHA-256 of the `.tv` bytes: catches a file swapped in from elsewhere.
    pub index_digest: String,
    /// SHA-256 of the id-map bytes.
    pub id_map_digest: String,
}

impl CompressedIndexMeta {
    /// Prove the sidecar describes exactly these two files.
    fn validate_pair(
        &self,
        index_path: &str,
        id_map_path: &str,
        index: &quantizer::TurboQuantVectorIndex,
        dimensions: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == COMPRESSED_INDEX_META_VERSION,
            "unsupported sidecar version {}",
            self.version
        );
        anyhow::ensure!(
            index.slots() == index.ids().len()
                && self.vectors == index.slots()
                && self.id_map_len == index.ids().len(),
            "sidecar describes {} vectors / {} ids but the files hold {} vectors / {} ids",
            self.vectors,
            self.id_map_len,
            index.slots(),
            index.ids().len()
        );
        anyhow::ensure!(
            self.dimensions == dimensions && index.dimensions() == dimensions,
            "index is {}-dimensional but this engine embeds {dimensions}d",
            index.dimensions()
        );
        anyhow::ensure!(
            self.bit_width == index.bit_width(),
            "sidecar says {}-bit but the index file is {}-bit",
            self.bit_width,
            index.bit_width()
        );
        anyhow::ensure!(
            self.index_digest == file_digest(index_path)?,
            "{index_path} was replaced after the sidecar was written"
        );
        anyhow::ensure!(
            self.id_map_digest == file_digest(id_map_path)?,
            "{id_map_path} was replaced after the sidecar was written"
        );
        Ok(())
    }
}

fn file_digest(path: &str) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Fingerprint of the corpus content an index was built for.
///
/// Covers every memory's ID and stored content, order-independently. Content is
/// what `edit_memory` rewrites and what a rebuild re-embeds, so an edit invalidates
/// the pair even when `source_hash` is left alone. A vector changed without a
/// content change is not representable here; Qdrant stays authoritative for the
/// payload, so that only costs ranking fidelity, never visibility.
pub(crate) fn corpus_fingerprint(memories: &[crate::storage::qdrant::MemoryPoint]) -> String {
    fingerprint_pairs(memories.iter().map(|m| (m.id.as_str(), m.content.as_str())))
}

fn fingerprint_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    use sha2::{Digest, Sha256};
    let mut pairs: Vec<(&str, &str)> = pairs.collect();
    pairs.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"memex8-compressed-index-v1");
    for (id, content) in pairs {
        hasher.update((id.len() as u64).to_le_bytes());
        hasher.update(id.as_bytes());
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn read_compressed_index_meta(index_path: &str) -> anyhow::Result<Option<CompressedIndexMeta>> {
    let path = compressed_index_meta_path(index_path);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(serde_json::from_str(&text)?))
}

/// Build the compressed search index from the live corpus and persist it.
///
/// This is the *only* writer of the index files, and every caller runs it after
/// its last corpus mutation: an index written before a consolidation would point
/// at deleted sources and miss the summaries that replaced them.
///
/// The pair is written to temporary siblings and renamed, so a crash mid-write
/// leaves either the previous pair or nothing — never a new index beside an old
/// ID map. An empty corpus, or a failed build, removes the previous files rather
/// than leaving an index that no longer describes the corpus; search falls back
/// to Qdrant, which is complete.
pub(crate) async fn rebuild_compressed_index(
    store: &QdrantStore,
    config: &AppConfig,
) -> anyhow::Result<usize> {
    let (index_path, id_map_path) = compressed_index_paths(config);
    // The fingerprint covers the whole point set (including memories without a
    // usable vector, which stay out of the index) so build-time and load-time
    // fingerprints are computed from the same view.
    let corpus = match store.scroll_all_memories().await {
        Ok(corpus) => corpus,
        Err(e) => {
            let _ = dedup_removal::invalidate_index_files(&index_path, &id_map_path);
            return Err(e);
        }
    };
    let built = build_compressed_index(store, config.turbovec.bit_width).await;
    let index = match built {
        Ok(Some(index)) => index,
        Ok(None) => {
            dedup_removal::invalidate_index_files(&index_path, &id_map_path)?;
            tracing::info!("  No memories to index; removed the compressed index");
            return Ok(0);
        }
        Err(e) => {
            dedup_removal::invalidate_index_files(&index_path, &id_map_path)?;
            return Err(e);
        }
    };
    if let Err(e) = save_compressed_index(
        &index,
        &index_path,
        &id_map_path,
        &corpus_fingerprint(&corpus),
    ) {
        let _ = dedup_removal::invalidate_index_files(&index_path, &id_map_path);
        return Err(e);
    }
    tracing::info!(
        "  Saved TurboVec index: {} vectors → {} ({:.1}x compression)",
        index.vector_count(),
        index_path,
        index.compression_ratio()
    );
    Ok(index.vector_count())
}

/// Compress every memory that has a usable vector. Memories without one cannot
/// be indexed; they are counted and warned about, and the resulting ID-set
/// mismatch at load time keeps search on Qdrant rather than silently omitting
/// them.
async fn build_compressed_index(
    store: &QdrantStore,
    bit_width: usize,
) -> anyhow::Result<Option<quantizer::TurboQuantVectorIndex>> {
    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    let mut dims = 0usize;
    let mut skipped = 0usize;
    for memory in store.scroll_all_memories_with_vectors().await? {
        if memory.vector.is_empty() {
            skipped += 1;
            continue;
        }
        if dims == 0 {
            dims = memory.vector.len();
        }
        if memory.vector.len() != dims {
            skipped += 1;
            continue;
        }
        rows.push((memory.memory.id, memory.vector));
    }
    if skipped > 0 {
        tracing::warn!(
            "  {} memories have no usable embedding and stay out of the compressed index",
            skipped
        );
    }
    if rows.is_empty() {
        return Ok(None);
    }
    let mut index = quantizer::TurboQuantVectorIndex::new(dims, bit_width);
    for chunk in rows.chunks(256) {
        let ids: Vec<String> = chunk.iter().map(|(id, _)| id.clone()).collect();
        let vectors: Vec<Vec<f32>> = chunk.iter().map(|(_, v)| v.clone()).collect();
        index.add_batch(&ids, &vectors);
    }
    Ok(Some(index))
}

fn save_compressed_index(
    index: &quantizer::TurboQuantVectorIndex,
    index_path: &str,
    id_map_path: &str,
    fingerprint: &str,
) -> anyhow::Result<()> {
    for path in [index_path, id_map_path] {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let temp_index = format!("{}.tmp", index_path);
    let temp_ids = format!("{}.tmp", id_map_path);
    if let Err(e) = index.save(&temp_index, &temp_ids) {
        let _ = std::fs::remove_file(&temp_index);
        let _ = std::fs::remove_file(&temp_ids);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp_index, index_path)
        .and_then(|()| std::fs::rename(&temp_ids, id_map_path))
    {
        let _ = std::fs::remove_file(&temp_index);
        let _ = std::fs::remove_file(&temp_ids);
        return Err(e.into());
    }
    // Written last, and only after the pair is in place: the digests describe
    // exactly the bytes now on disk. A pair without a sidecar is never loaded.
    let meta = CompressedIndexMeta {
        version: COMPRESSED_INDEX_META_VERSION,
        vectors: index.vector_count(),
        id_map_len: index.ids().len(),
        dimensions: index.dimensions(),
        bit_width: index.bit_width(),
        fingerprint: fingerprint.to_string(),
        index_digest: file_digest(index_path)?,
        id_map_digest: file_digest(id_map_path)?,
    };
    let meta_path = compressed_index_meta_path(index_path);
    let temp_meta = format!("{meta_path}.tmp");
    std::fs::write(&temp_meta, serde_json::to_vec(&meta)?)?;
    if let Err(e) = std::fs::rename(&temp_meta, &meta_path) {
        let _ = std::fs::remove_file(&temp_meta);
        return Err(e.into());
    }
    // A successful rebuild supersedes any failure marker from an earlier write.
    let _ = std::fs::remove_file(compressed_index_stale_marker(index_path));
    Ok(())
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
mod turbovec_freshness_tests {
    use super::{
        compressed_index_paths, fingerprint_pairs, read_compressed_index_meta,
        save_compressed_index, turbovec_index_matches_corpus, CompressedIndexMeta,
    };
    use crate::config::AppConfig;
    use crate::engine::quantizer::TurboQuantVectorIndex;
    use std::collections::HashSet;

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn indexed(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn fingerprint(pairs: &[(&str, &str)]) -> String {
        fingerprint_pairs(pairs.iter().copied())
    }

    fn vector(dims: usize, seed: f32) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dims).map(|i| ((i as f32) + seed).sin()).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memex8-index-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A rebuilt index describes exactly the corpus it was built for.
    #[test]
    fn identical_id_sets_are_accepted() {
        assert!(turbovec_index_matches_corpus(
            &indexed(&["a", "b", "c"]),
            &live(&["a", "b", "c"])
        ));
        assert!(turbovec_index_matches_corpus(&[], &live(&[])));
    }

    /// The consolidation regression: sources `a` and `b` are deleted and summary
    /// `s` is inserted. Three vectors were live before and two after, but with a
    /// one-for-one swap the count still matches — so a count-only freshness check
    /// accepted an index that could not answer for `s` and still resolved `a`.
    #[test]
    fn replaced_ids_with_an_equal_count_are_rejected() {
        let index = indexed(&["a", "b"]);
        assert_eq!(index.len(), live(&["s", "b"]).len());
        assert!(!turbovec_index_matches_corpus(&index, &live(&["s", "b"])));
    }

    /// An index that is larger than the corpus (deleted IDs still resident) is
    /// stale even though it "covers" every live memory.
    #[test]
    fn deleted_ids_are_rejected_even_when_every_live_id_is_present() {
        assert!(!turbovec_index_matches_corpus(
            &indexed(&["a", "b", "deleted"]),
            &live(&["a", "b"])
        ));
    }

    /// Memories ingested after the last rebuild are not in the index, which is
    /// exactly the store-then-search failure.
    #[test]
    fn live_ids_missing_from_the_index_are_rejected() {
        assert!(!turbovec_index_matches_corpus(
            &indexed(&["a", "b"]),
            &live(&["a", "b", "new"])
        ));
    }

    /// Empty config paths resolve to the same defaults slumber always used, so
    /// invalidation and rebuild touch the same files.
    #[test]
    fn empty_paths_use_the_data_defaults() {
        let mut config = AppConfig::default();
        config.turbovec.index_path = String::new();
        config.turbovec.id_map_path = String::new();
        assert_eq!(
            compressed_index_paths(&config),
            (
                "data/memories.tv".to_string(),
                "data/memories_ids.json".to_string()
            )
        );
        config.turbovec.index_path = "/tmp/x.tv".into();
        config.turbovec.id_map_path = "/tmp/x-ids.json".into();
        assert_eq!(
            compressed_index_paths(&config),
            ("/tmp/x.tv".to_string(), "/tmp/x-ids.json".to_string())
        );
    }

    /// The ID set can be unchanged while every stored vector is stale — exactly
    /// what `edit_memory` leaves behind, since it rewrites the content and the
    /// vector but never `source_hash`. The fingerprint must follow the content.
    #[test]
    fn fingerprint_changes_when_content_changes_with_unchanged_ids() {
        let before = fingerprint(&[("a", "original text"), ("b", "other text")]);
        let after = fingerprint(&[("a", "edited text"), ("b", "other text")]);
        assert_eq!(live(&["a", "b"]).len(), 2);
        assert_ne!(before, after);
    }

    /// Scroll order is not part of the corpus, so it must not move the hash.
    #[test]
    fn fingerprint_is_order_independent() {
        assert_eq!(
            fingerprint(&[("a", "one"), ("b", "two")]),
            fingerprint(&[("b", "two"), ("a", "one")])
        );
    }

    /// Fields are length-prefixed, so a boundary shift cannot alias two corpora.
    #[test]
    fn fingerprint_separates_field_boundaries() {
        assert_ne!(fingerprint(&[("ab", "c")]), fingerprint(&[("a", "bc")]));
    }

    fn pair_on_disk(tag: &str, rows: &[(&str, Vec<f32>)]) -> (std::path::PathBuf, String, String) {
        let dir = tmp_dir(tag);
        let index_path = dir.join("corpus.tv").to_string_lossy().to_string();
        let id_map_path = dir.join("corpus-ids.json").to_string_lossy().to_string();
        let mut index = TurboQuantVectorIndex::new(8, 4);
        let ids: Vec<String> = rows.iter().map(|(id, _)| id.to_string()).collect();
        let vectors: Vec<Vec<f32>> = rows.iter().map(|(_, v)| v.clone()).collect();
        index.add_batch(&ids, &vectors);
        save_compressed_index(&index, &index_path, &id_map_path, "fingerprint-of-build").unwrap();
        (dir, index_path, id_map_path)
    }

    /// The pair as written validates against its own sidecar.
    #[test]
    fn sidecar_validates_the_pair_it_describes() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-ok", &rows);
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        assert_eq!(meta.vectors, 2);
        assert_eq!(meta.id_map_len, 2);
        assert_eq!(meta.fingerprint, "fingerprint-of-build");
        meta.validate_pair(&index_path, &id_map_path, &loaded, 8)
            .expect("a freshly written pair must validate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `.tv` swapped in from another corpus whose slot count matches the id map
    /// passes the length check, so the sidecar digests are what catch it.
    #[test]
    fn sidecar_rejects_a_foreign_index_of_the_same_length() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-swap", &rows);
        let (_other, other_index, _other_ids) = pair_on_disk(
            "meta-swap-foreign",
            &[("x", vector(8, 3.0)), ("y", vector(8, 4.0))],
        );
        std::fs::copy(&other_index, &index_path).unwrap();
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        assert_eq!(
            loaded.slots(),
            2,
            "same length: the count check cannot help"
        );
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        let error = meta
            .validate_pair(&index_path, &id_map_path, &loaded, 8)
            .unwrap_err()
            .to_string();
        assert!(error.contains("was replaced"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&_other);
    }

    /// A mixed pair whose lengths agree with each other but not with the sidecar
    /// is still refused.
    #[test]
    fn sidecar_rejects_a_pair_of_a_different_size() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-size", &rows);
        let (_other, other_index, other_ids) = pair_on_disk(
            "meta-size-foreign",
            &[
                ("x", vector(8, 3.0)),
                ("y", vector(8, 4.0)),
                ("z", vector(8, 5.0)),
            ],
        );
        std::fs::copy(&other_index, &index_path).unwrap();
        std::fs::copy(&other_ids, &id_map_path).unwrap();
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        assert_eq!(loaded.slots(), 3);
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        let error = meta
            .validate_pair(&index_path, &id_map_path, &loaded, 8)
            .unwrap_err()
            .to_string();
        assert!(error.contains("sidecar describes 2 vectors"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&_other);
    }

    /// The sidecar lives beside the pair and must be named off the index path, so
    /// invalidation and rebuild always hit the same file.
    #[test]
    fn sidecar_and_marker_paths_derive_from_the_index_path() {
        assert_eq!(
            super::compressed_index_meta_path("/data/m.tv"),
            "/data/m.tv.meta.json"
        );
        assert_eq!(
            super::compressed_index_stale_marker("/data/m.tv"),
            "/data/m.tv.stale"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::verification_score_multiplier;
    use super::{read_slumber_pause, write_slumber_pause};

    fn scratch_flag(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "memex8-slumber-pause-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("slumber_paused.json")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn slumber_pause_flag_is_durable_and_fails_open() {
        let path = scratch_flag("roundtrip");
        // A missing flag must mean "not paused": otherwise a fresh data volume
        // could never run maintenance.
        assert!(!read_slumber_pause(&path));
        write_slumber_pause(&path, true).expect("write pause");
        assert!(
            read_slumber_pause(&path),
            "persisted pause was not readable"
        );
        assert!(
            !std::path::Path::new(&format!("{path}.tmp")).exists(),
            "atomic write left a temporary file behind"
        );
        write_slumber_pause(&path, false).expect("write resume");
        assert!(!read_slumber_pause(&path));
        // A truncated flag fails open (with a warning) instead of disabling
        // maintenance silently and forever.
        std::fs::write(&path, b"{not json").expect("corrupt flag");
        assert!(!read_slumber_pause(&path));
        std::fs::remove_file(&path).ok();
    }

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
