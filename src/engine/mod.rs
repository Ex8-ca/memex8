pub mod compressor;
pub mod chunker;
pub mod doctor;
pub mod embedder;
pub mod graph;
pub mod ingester;
pub mod memex8_md;
pub mod providers;
pub mod quantizer;
pub mod realms;
pub mod search;
pub mod slumber;

use crate::config::AppConfig;
use crate::storage::qdrant::QdrantStore;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlumberStatus {
    pub state: String, // "idle", "running", "paused"
    pub last_run: Option<String>,
    pub next_scheduled: Option<String>,
    pub memories_processed: u64,
    pub realms_reorganized: u32,
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
}

struct SlumberState {
    status: String,
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    last_query: chrono::DateTime<chrono::Utc>,
    memories_processed: u64,
    realms_reorganized: u32,
}

impl Engine {
    pub async fn new(config: AppConfig) -> anyhow::Result<Self> {
        let store = QdrantStore::new(&config.qdrant.url).await?;
        store.ensure_collections(config.embedding_dimensions()).await?;
        tracing::info!(
            "Engine initialized: {} embeddings, {}d",
            config.embedding.provider,
            config.embedding.dimensions
        );
        Ok(Self {
            config,
            store,
            slumber_state: Arc::new(RwLock::new(SlumberState {
                status: "idle".into(),
                last_run: None,
                last_query: chrono::Utc::now(),
                memories_processed: 0,
                realms_reorganized: 0,
            })),
        })
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

        let embedder = embedder::create_embedder(&self.config)?;
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

            self.store.store_memory(
                &id,
                &vector,
                &chunk.content,
                chunk.heading.as_deref(),
                Some(chunk.source_file.as_str()),
                &realm_id,
                &realm_name,
                &chunk.source_hash,
                &chunk.chunk_type,
            ).await?;

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
            self.store.store_realm(&id, vector, &name, None, false).await?;
            return Ok(id);
        }

        // Find closest realm by cosine similarity
        let mut best_realm = None;
        let mut best_score = -1.0f32;
        for realm in &realms {
            let score = cosine_similarity(vector, &realm.centroid);
            if score > best_score {
                best_score = score;
                best_realm = Some(realm.clone());
            }
        }

        if let Some(realm) = best_realm {
            if best_score >= self.config.realms.similarity_threshold {
                return Ok(realm.id);
            }
        }

        // No close realm — create new one
        let id = uuid::Uuid::new_v4().to_string();
        let name = format!("realm-{}", &id[..8]);
        self.store.store_realm(&id, vector, &name, None, false).await?;
        Ok(id)
    }

    pub async fn watch_path(&self, path: &str) -> anyhow::Result<()> {
        tracing::info!("Watching path: {}", path);
        // TODO: implement file watcher with notify crate
        Ok(())
    }

    pub async fn watch_add(
        &self,
        path: &str,
        poll_interval: &str,
        realm_hint: Option<&str>,
        chunk_by: &str,
    ) -> anyhow::Result<()> {
        // TODO: persist watch config
        tracing::info!("Added watch: {} (poll: {}, chunk: {})", path, poll_interval, chunk_by);
        Ok(())
    }

    pub async fn watch_list(&self) -> anyhow::Result<()> {
        // TODO: list persisted watches
        println!("No watches configured yet.");
        Ok(())
    }

    pub async fn watch_remove(&self, path: &str) -> anyhow::Result<()> {
        // TODO: remove persisted watch
        Ok(())
    }

    pub async fn search(
        &self,
        query: &str,
        realm: Option<&str>,
        limit: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        let embedder = embedder::create_embedder(&self.config)?;
        let query_vector = embedder.embed(query).await?;

        let results = self.store.search(&query_vector, limit, min_score, realm).await?;

        {
            let mut state = self.slumber_state.write().await;
            state.last_query = chrono::Utc::now();
        }

        Ok(results.into_iter().map(|r| MemoryResult {
            id: r.payload.id,
            content: r.payload.content,
            heading: r.payload.heading,
            realm_name: r.payload.realm_name,
            importance: r.payload.importance,
            score: r.score,
            last_accessed: r.payload.last_accessed,
            access_count: r.payload.access_count,
        }).collect())
    }

    pub async fn get_memory(&self, id: &str) -> anyhow::Result<crate::storage::qdrant::MemoryPoint> {
        self.store.get_memory(id).await?.ok_or_else(|| anyhow::anyhow!("Memory not found: {}", id))
    }

    pub async fn recall(&self, limit: usize, realm: Option<&str>) -> anyhow::Result<Vec<MemoryResult>> {
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
                let score = m.importance * recency as f32 * (1.0 + m.access_count as f32 * 0.05);
                (m, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

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
            })
            .collect())
    }

    pub async fn list_realms(&self) -> anyhow::Result<Vec<crate::storage::qdrant::RealmPoint>> {
        self.store.list_realms().await
    }

    pub async fn create_realm(&self, name: &str, description: Option<&str>) -> anyhow::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        // Use a zero vector as initial centroid; will be updated during slumber
        let centroid = vec![0.0; self.config.embedding.dimensions as usize];
        self.store.store_realm(&id, &centroid, name, description, true).await
    }

    pub async fn show_realm(&self, name: &str) -> anyhow::Result<crate::storage::qdrant::RealmPoint> {
        self.store.find_realm_by_name(name).await?.ok_or_else(|| anyhow::anyhow!("Realm not found: {}", name))
    }

    pub async fn merge_realms(&self, target: &str, source: &str) -> anyhow::Result<()> {
        let target_realm = self.store.find_realm_by_name(target).await?
            .ok_or_else(|| anyhow::anyhow!("Target realm not found: {}", target))?;
        let source_realm = self.store.find_realm_by_name(source).await?
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
        self.store.update_upvotes(id, new_upvotes, new_importance).await
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
        let embedder = embedder::create_embedder(&self.config)?;
        let vector = embedder.embed(new_content).await?;

        let existing = self.get_memory(id).await?;
        let realm_id = existing.realm_id.as_deref().unwrap_or("");
        self.store.store_memory(
            id,
            &vector,
            new_content,
            existing.heading.as_deref(),
            existing.source_file.as_deref(),
            realm_id,
            &existing.realm_name,
            &existing.source_hash,
            &existing.chunk_type,
        ).await?;

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
        }
    }

    pub async fn trigger_slumber(&self) -> anyhow::Result<()> {
        {
            let mut state = self.slumber_state.write().await;
            state.status = "running".into();
        }

        tracing::info!("💤 Slumber started...");
        let slumber = slumber::SlumberEngine::new(
            self.config.clone(),
            self.store.clone_store(),
        );
        slumber.run_full_pipeline().await?;

        {
            let mut state = self.slumber_state.write().await;
            state.status = "idle".into();
            state.last_run = Some(chrono::Utc::now());
            state.realms_reorganized += 1;
        }
        tracing::info!("✅ Slumber complete.");
        Ok(())
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
        let mem_stats = self.store.get_collection_stats(&self.config.qdrant.collection_memories).await?;
        let realms = self.store.list_realms().await?;
        let slumber = self.slumber_status().await;

        Ok(SystemStats {
            total_memories: mem_stats.vector_count,
            total_realms: realms.len() as u32,
            storage_bytes: mem_stats.size_bytes,
            embedding_provider: self.config.embedding.provider.clone(),
            embedding_model: self.config.embedding.model.clone(),
            embedding_dimensions: self.config.embedding.dimensions,
            slumber_state: slumber.state,
        })
    }

    pub async fn store_memory(
        &self,
        content: &str,
        tags: Option<Vec<String>>,
        realm_hint: Option<&str>,
        source: Option<&str>,
    ) -> anyhow::Result<String> {
        let embedder = embedder::create_embedder(&self.config)?;
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

        self.store.store_memory(
            &id,
            &vector,
            content,
            None,
            Some(source_str),
            &realm_id,
            &realm_name,
            "",
            "manual",
        ).await?;

        Ok(id)
    }

    pub async fn graph_search(
        &self,
        entity: &str,
        relationship: Option<&str>,
        depth: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        // TODO: implement knowledge graph traversal
        // For now, fall back to semantic search and wrap results
        let embedder = embedder::create_embedder(&self.config)?;
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
        let memories = self.store.scroll_all_memories().await?;
        let json = serde_json::to_string_pretty(&memories)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub async fn import(&self, path: &str) -> anyhow::Result<usize> {
        let content = std::fs::read_to_string(path)?;
        let memories: Vec<crate::storage::qdrant::MemoryPoint> = serde_json::from_str(&content)?;
        let count = memories.len();

        let embedder = embedder::create_embedder(&self.config)?;
        for mem in &memories {
            let vector = embedder.embed(&mem.content).await?;
            let realm_id = mem.realm_id.as_deref().unwrap_or("");
            self.store.store_memory(
                &mem.id,
                &vector,
                &mem.content,
                mem.heading.as_deref(),
                mem.source_file.as_deref(),
                realm_id,
                &mem.realm_name,
                &mem.source_hash,
                &mem.chunk_type,
            ).await?;
        }

        tracing::info!("Imported {} memories from {}", count, path);
        Ok(count)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}
