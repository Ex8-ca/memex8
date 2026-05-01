use qdrant_client::qdrant::{
    point_id::PointIdOptions, points_selector::PointsSelectorOneOf, vectors_output::VectorsOptions,
    Condition, CountPointsBuilder, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeletePointsBuilder, Distance, FieldType, Filter, GetPointsBuilder, PointStruct, PointsIdsList,
    ScrollPointsBuilder, SearchPointsBuilder, SetPayloadPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder,
};
use qdrant_client::Payload;
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};

// ─── Data types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPoint {
    pub id: String,
    pub content: String,
    pub summary: Option<String>,
    pub source_file: Option<String>,
    pub realm_id: Option<String>,
    pub realm_name: String,
    pub importance: f32,
    pub upvotes: u32,
    pub tags: Vec<String>,
    pub ingested_at: String,
    pub last_accessed: String,
    pub access_count: u32,
    pub chunk_type: String,
    pub heading: Option<String>,
    pub source_hash: String,
    /// IDs of semantically associated memories.
    #[serde(default)]
    pub related_memory_ids: Vec<String>,
    /// Cosine similarity strengths for each related memory (same order as related_memory_ids).
    #[serde(default)]
    pub association_strengths: Vec<f32>,
}

/// Memory with its embedding vector (internal use only, not serialized).
#[derive(Debug, Clone)]
pub struct MemoryPointWithVector {
    pub memory: MemoryPoint,
    pub vector: Option<Vec<f32>>,
}

/// Memory with vector — the public API for slumber compression.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryWithVector {
    #[serde(flatten)]
    pub memory: MemoryPoint,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmPoint {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub centroid: Vec<f32>,
    pub memory_count: u32,
    pub is_user_pinned: bool,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub payload: MemoryPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionStats {
    pub vector_count: u64,
    pub size_bytes: u64,
}

// ─── Store ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct QdrantStore {
    client: Qdrant,
}

const MEMORIES: &str = "memories";
const REALMS: &str = "realms";
const QUANTIZED: &str = "memories_quantized";

/// Helper: convert PointId to String.
fn point_id_to_string(id: Option<&qdrant_client::qdrant::PointId>) -> String {
    match id {
        Some(pid) => match &pid.point_id_options {
            Some(PointIdOptions::Num(n)) => n.to_string(),
            Some(PointIdOptions::Uuid(s)) => s.clone(),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// Extract dense vector from a RetrievedPoint.
#[allow(deprecated)]
fn extract_vector(point: &qdrant_client::qdrant::RetrievedPoint) -> Option<Vec<f32>> {
    point.vectors.as_ref().and_then(|v| {
        v.vectors_options.as_ref().and_then(|opts| match opts {
            VectorsOptions::Vector(vec_out) => {
                // Try the newer vector field first, fall back to deprecated data
                vec_out
                    .vector
                    .as_ref()
                    .and_then(|v| match v {
                        qdrant_client::qdrant::vector_output::Vector::Dense(d) => {
                            Some(d.data.iter().map(|x| *x as f32).collect())
                        }
                        qdrant_client::qdrant::vector_output::Vector::Sparse(_s) => None,
                        qdrant_client::qdrant::vector_output::Vector::MultiDense(_m) => None,
                    })
                    .or_else(|| {
                        // Fallback to deprecated data field
                        if !vec_out.data.is_empty() {
                            Some(vec_out.data.iter().map(|x| *x as f32).collect())
                        } else {
                            None
                        }
                    })
            }
            VectorsOptions::Vectors(named) => {
                // Get first named vector
                named.vectors.values().next().and_then(|v| {
                    v.vector.as_ref().and_then(|v| match v {
                        qdrant_client::qdrant::vector_output::Vector::Dense(d) => {
                            Some(d.data.iter().map(|x| *x as f32).collect())
                        }
                        qdrant_client::qdrant::vector_output::Vector::Sparse(_s) => None,
                        qdrant_client::qdrant::vector_output::Vector::MultiDense(_m) => None,
                    })
                })
            }
        })
    })
}

/// Helper: convert a raw HashMap<String, Value> to serde_json map.
fn map_to_json(
    raw: &std::collections::HashMap<String, qdrant_client::qdrant::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let payload = Payload::from(raw.clone());
    let json_val: serde_json::Value = payload.into();
    match json_val {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn map_str(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn map_f32(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> f32 {
    map.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32
}

fn map_u32(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> u32 {
    map.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32
}

fn map_bool(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    map.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn map_tags(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn map_str_vec(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn map_f32_vec(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<f32> {
    map.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_f64())
                .map(|f| f as f32)
                .collect()
        })
        .unwrap_or_default()
}

// ─── MemoryPoint helpers ──────────────────────────────────────────────────────

fn memory_to_payload(mem: &MemoryPoint) -> Payload {
    let json = serde_json::json!({
        "content": mem.content,
        "summary": mem.summary,
        "source_file": mem.source_file,
        "realm_id": mem.realm_id,
        "realm_name": mem.realm_name,
        "importance": mem.importance,
        "upvotes": mem.upvotes,
        "tags": mem.tags,
        "ingested_at": mem.ingested_at,
        "last_accessed": mem.last_accessed,
        "access_count": mem.access_count,
        "chunk_type": mem.chunk_type,
        "heading": mem.heading,
        "source_hash": mem.source_hash,
        "related_memory_ids": mem.related_memory_ids,
        "association_strengths": mem.association_strengths,
    });
    Payload::try_from(json).unwrap_or_default()
}

fn memory_from_payload(id: &str, map: &serde_json::Map<String, serde_json::Value>) -> MemoryPoint {
    MemoryPoint {
        id: id.to_string(),
        content: map_str(map, "content").unwrap_or_default(),
        summary: map_str(map, "summary"),
        source_file: map_str(map, "source_file"),
        realm_id: map_str(map, "realm_id"),
        realm_name: map_str(map, "realm_name").unwrap_or_default(),
        importance: map_f32(map, "importance"),
        upvotes: map_u32(map, "upvotes"),
        tags: map_tags(map, "tags"),
        ingested_at: map_str(map, "ingested_at").unwrap_or_default(),
        last_accessed: map_str(map, "last_accessed").unwrap_or_default(),
        access_count: map_u32(map, "access_count"),
        chunk_type: map_str(map, "chunk_type").unwrap_or_default(),
        heading: map_str(map, "heading"),
        source_hash: map_str(map, "source_hash").unwrap_or_default(),
        related_memory_ids: map_str_vec(map, "related_memory_ids"),
        association_strengths: map_f32_vec(map, "association_strengths"),
    }
}

// ─── RealmPoint helpers ───────────────────────────────────────────────────────

fn realm_to_payload(realm: &RealmPoint) -> Payload {
    let centroid_arr: Vec<serde_json::Value> = realm
        .centroid
        .iter()
        .map(|v| serde_json::json!(v))
        .collect();
    let json = serde_json::json!({
        "name": realm.name,
        "description": realm.description,
        "memory_count": realm.memory_count,
        "is_user_pinned": realm.is_user_pinned,
        "centroid": centroid_arr,
    });
    Payload::try_from(json).unwrap_or_default()
}

fn realm_from_payload(
    id: &str,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<RealmPoint> {
    let name = map_str(map, "name")?;
    let centroid = map
        .get("centroid")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_f64())
                .map(|v| v as f32)
                .collect()
        })
        .unwrap_or_default();
    Some(RealmPoint {
        id: id.to_string(),
        name,
        description: map_str(map, "description"),
        memory_count: map_u32(map, "memory_count"),
        is_user_pinned: map_bool(map, "is_user_pinned"),
        centroid,
    })
}

// ─── QdrantStore implementation ───────────────────────────────────────────────

impl QdrantStore {
    pub async fn new(url: &str) -> anyhow::Result<Self> {
        tracing::info!("Connecting to Qdrant at {}", url);
        let client = Qdrant::from_url(url).build()?;
        Ok(Self { client })
    }

    pub async fn ensure_collections(&self, dimensions: u32) -> anyhow::Result<()> {
        tracing::info!(
            "Ensuring Qdrant collections exist ({} dimensions)...",
            dimensions
        );
        let dims = dimensions as u64;

        // ── memories ──
        if !self.client.collection_exists(MEMORIES).await? {
            tracing::info!("Creating {} collection", MEMORIES);
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(MEMORIES)
                        .vectors_config(VectorParamsBuilder::new(dims, Distance::Cosine))
                        .on_disk_payload(true),
                )
                .await?;

            for (field, schema) in &[
                ("realm_name", FieldType::Keyword),
                ("tags", FieldType::Keyword),
                ("chunk_type", FieldType::Keyword),
                ("importance", FieldType::Float),
            ] {
                self.client
                    .create_field_index(CreateFieldIndexCollectionBuilder::new(
                        MEMORIES, *field, *schema,
                    ))
                    .await?;
            }
            tracing::info!("  + indexes created for {}", MEMORIES);
        }

        // ── realms ──
        if !self.client.collection_exists(REALMS).await? {
            tracing::info!("Creating {} collection", REALMS);
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(REALMS)
                        .vectors_config(VectorParamsBuilder::new(
                            dimensions as u64,
                            Distance::Cosine,
                        ))
                        .on_disk_payload(true),
                )
                .await?;
            self.client
                .create_field_index(CreateFieldIndexCollectionBuilder::new(
                    REALMS,
                    "name",
                    FieldType::Keyword,
                ))
                .await?;
        }

        // ── quantized ──
        if !self.client.collection_exists(QUANTIZED).await? {
            tracing::info!("Creating {} collection", QUANTIZED);
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(QUANTIZED)
                        .vectors_config(VectorParamsBuilder::new(dims, Distance::Cosine))
                        .on_disk_payload(true),
                )
                .await?;
        }

        Ok(())
    }

    // ── memories CRUD ─────────────────────────────────────────────────────────

    pub async fn store_memory(
        &self,
        id: &str,
        vector: &[f32],
        content: &str,
        heading: Option<&str>,
        source_file: Option<&str>,
        realm_id: &str,
        realm_name: &str,
        source_hash: &str,
        chunk_type: &str,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mem = MemoryPoint {
            id: id.to_string(),
            content: content.to_string(),
            summary: None,
            source_file: source_file.map(|s| s.to_string()),
            realm_id: Some(realm_id.to_string()),
            realm_name: realm_name.to_string(),
            importance: 1.0,
            upvotes: 0,
            tags: vec![],
            ingested_at: now.clone(),
            last_accessed: now,
            access_count: 0,
            chunk_type: chunk_type.to_string(),
            heading: heading.map(|s| s.to_string()),
            source_hash: source_hash.to_string(),
            related_memory_ids: vec![],
            association_strengths: vec![],
        };
        let payload = memory_to_payload(&mem);
        let point = PointStruct::new(id.to_string(), vector.to_vec(), payload);

        self.client
            .upsert_points(UpsertPointsBuilder::new(MEMORIES, vec![point]).wait(true))
            .await?;
        Ok(())
    }

    pub async fn get_memory(&self, id: &str) -> anyhow::Result<Option<MemoryPoint>> {
        let resp = self
            .client
            .get_points(
                GetPointsBuilder::new(MEMORIES, vec![id.into()])
                    .with_payload(true)
                    .with_vectors(false),
            )
            .await?;

        if let Some(point) = resp.result.into_iter().next() {
            let pid = point_id_to_string(point.id.as_ref());
            let map = map_to_json(&point.payload);
            return Ok(Some(memory_from_payload(&pid, &map)));
        }
        Ok(None)
    }

    pub async fn search(
        &self,
        query_vector: &[f32],
        limit: usize,
        min_score: f32,
        realm_filter: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut builder = SearchPointsBuilder::new(MEMORIES, query_vector, limit as u64)
            .with_payload(true)
            .with_vectors(false);

        if let Some(realm) = realm_filter {
            builder = builder.filter(Filter::must([Condition::matches(
                "realm_name",
                realm.to_string(),
            )]));
        }

        let resp = self.client.search_points(builder).await?;

        Ok(resp
            .result
            .into_iter()
            .filter(|r| r.score >= min_score)
            .filter_map(|r| {
                let pid = point_id_to_string(r.id.as_ref());
                let map = map_to_json(&r.payload);
                let mem = memory_from_payload(&pid, &map);
                Some(SearchResult {
                    id: mem.id.clone(),
                    score: r.score,
                    payload: mem,
                })
            })
            .collect())
    }

    pub async fn delete_memory(&self, id: &str) -> anyhow::Result<()> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(MEMORIES)
                    .points(PointsIdsList {
                        ids: vec![id.into()],
                    })
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    /// Store a memory with a pre-computed vector (for consolidated summaries).
    pub async fn store_memory_with_vector(
        &self,
        id: &str,
        content: &str,
        vector: &[f32],
        realm_id: Option<&str>,
        realm_name: Option<&str>,
        importance: f32,
        source_file: Option<&str>,
    ) -> anyhow::Result<()> {
        let ingested_at = chrono::Utc::now().to_rfc3339();

        let payload: Payload = serde_json::json!({
            "content": content,
            "realm_id": realm_id,
            "realm_name": realm_name.unwrap_or("general"),
            "importance": importance,
            "upvotes": 0u32,
            "access_count": 0u32,
            "ingested_at": ingested_at,
            "last_accessed": ingested_at,
            "source_file": source_file.unwrap_or(""),
            "source_hash": "",
            "chunk_type": "consolidated",
            "related_memory_ids": Vec::<String>::new(),
            "association_strengths": Vec::<f32>::new(),
        })
        .try_into()
        .unwrap_or_default();

        let point = PointStruct::new(id.to_string(), vector.to_vec(), payload);
        self.client
            .upsert_points(UpsertPointsBuilder::new(MEMORIES, vec![point]).wait(true))
            .await?;
        Ok(())
    }

    pub async fn update_upvotes(
        &self,
        id: &str,
        upvotes: u32,
        importance: f32,
    ) -> anyhow::Result<()> {
        let payload: Payload = serde_json::json!({
            "upvotes": upvotes,
            "importance": importance,
        })
        .try_into()
        .unwrap_or_default();

        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(MEMORIES, payload)
                    .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: vec![id.into()],
                    }))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    /// Touch a memory: increment access_count, update last_accessed, bump importance.
    /// This is the core of the "human memory" model — frequently recalled memories
    /// become stronger and more likely to surface in future searches.
    pub async fn track_access(&self, id: &str, importance_bump: f32) -> anyhow::Result<()> {
        let current = self.get_memory(id).await?;
        let (new_access_count, new_importance) = if let Some(mem) = current {
            let count = mem.access_count + 1;
            let importance = (mem.importance + importance_bump).min(1.0);
            (count, importance)
        } else {
            return Ok(());
        };

        let now = chrono::Utc::now().to_rfc3339();
        let payload: Payload = serde_json::json!({
            "last_accessed": now,
            "access_count": new_access_count,
            "importance": new_importance,
        })
        .try_into()
        .unwrap_or_default();

        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(MEMORIES, payload)
                    .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: vec![id.into()],
                    }))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    /// Touch multiple memories in a single batch (for search results).
    pub async fn track_access_batch(
        &self,
        ids: &[&str],
        importance_bump: f32,
    ) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let point_ids: Vec<qdrant_client::qdrant::PointId> =
            ids.iter().map(|id| (*id).into()).collect();
        let resp = self
            .client
            .get_points(
                GetPointsBuilder::new(MEMORIES, point_ids)
                    .with_payload(true)
                    .with_vectors(false),
            )
            .await?;

        let now = chrono::Utc::now().to_rfc3339();

        for point in resp.result {
            let pid = point_id_to_string(point.id.as_ref());
            let map = map_to_json(&point.payload);
            let mem = memory_from_payload(&pid, &map);

            let new_count = mem.access_count + 1;
            let new_importance = (mem.importance + importance_bump).min(1.0);

            let payload: Payload = serde_json::json!({
                "last_accessed": now,
                "access_count": new_count,
                "importance": new_importance,
            })
            .try_into()
            .unwrap_or_default();

            self.client
                .set_payload(
                    SetPayloadPointsBuilder::new(MEMORIES, payload)
                        .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                            ids: vec![pid.into()],
                        }))
                        .wait(false),
                )
                .await?;
        }

        Ok(())
    }

    pub async fn scroll_all_memories(&self) -> anyhow::Result<Vec<MemoryPoint>> {
        let results = self.scroll_memories_internal(false).await?;
        Ok(results.into_iter().map(|m| m.memory).collect())
    }

    /// Scroll all memories WITH their embedding vectors.
    /// Used by slumber for ScalarQuant compression.
    pub async fn scroll_all_memories_with_vectors(&self) -> anyhow::Result<Vec<MemoryWithVector>> {
        let raw = self.scroll_memories_internal(true).await?;
        Ok(raw
            .into_iter()
            .filter_map(|m| {
                m.vector.map(|v| MemoryWithVector {
                    memory: m.memory,
                    vector: v,
                })
            })
            .collect())
    }

    async fn scroll_memories_internal(
        &self,
        with_vectors: bool,
    ) -> anyhow::Result<Vec<MemoryPointWithVector>> {
        let mut memories = Vec::new();
        let mut offset: Option<String> = None;

        loop {
            let mut builder = ScrollPointsBuilder::new(MEMORIES)
                .limit(500)
                .with_payload(true);
            if with_vectors {
                builder = builder.with_vectors(true);
            }
            if let Some(ref off) = offset {
                builder = builder.offset(off.clone());
            }

            let resp = self.client.scroll(builder).await?;
            for point in resp.result {
                let pid = point_id_to_string(point.id.as_ref());
                let map = map_to_json(&point.payload);
                let vector = extract_vector(&point);

                memories.push(MemoryPointWithVector {
                    memory: memory_from_payload(&pid, &map),
                    vector,
                });
            }

            if resp.next_page_offset.is_none() {
                break;
            }
            offset = resp
                .next_page_offset
                .as_ref()
                .map(|p| point_id_to_string(Some(p)));
        }

        Ok(memories)
    }

    pub async fn count_memories(&self) -> anyhow::Result<u64> {
        let resp = self.client.count(CountPointsBuilder::new(MEMORIES)).await?;
        Ok(resp.result.map(|r| r.count as u64).unwrap_or(0))
    }

    pub async fn delete_by_realm(&self, realm_id: &str) -> anyhow::Result<()> {
        let filter = Filter::must([Condition::matches("realm_id", realm_id.to_string())]);
        self.client
            .delete_points(
                DeletePointsBuilder::new(MEMORIES)
                    .points(PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    pub async fn search_by_tags(
        &self,
        query_vector: &[f32],
        tags: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let filter = Filter::must(tags.iter().map(|t| Condition::matches("tags", t.clone())));

        let resp = self
            .client
            .search_points(
                SearchPointsBuilder::new(MEMORIES, query_vector, limit as u64)
                    .filter(filter)
                    .with_payload(true)
                    .with_vectors(false),
            )
            .await?;

        Ok(resp
            .result
            .into_iter()
            .filter_map(|r| {
                let pid = point_id_to_string(r.id.as_ref());
                let map = map_to_json(&r.payload);
                let mem = memory_from_payload(&pid, &map);
                Some(SearchResult {
                    id: mem.id.clone(),
                    score: r.score,
                    payload: mem,
                })
            })
            .collect())
    }

    /// Search for tag suggestions — returns the most common tags.
    pub async fn get_tag_suggestions(&self, limit: usize) -> anyhow::Result<Vec<(String, u32)>> {
        let all = self.scroll_all_memories().await?;
        let mut tag_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for mem in &all {
            for tag in &mem.tags {
                *tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
        let mut tags: Vec<_> = tag_counts.into_iter().collect();
        tags.sort_by(|a, b| b.1.cmp(&a.1));
        tags.truncate(limit);
        Ok(tags)
    }

    // ── realms CRUD ───────────────────────────────────────────────────────────

    pub async fn list_realms(&self) -> anyhow::Result<Vec<RealmPoint>> {
        let mut realms = Vec::new();
        let mut offset: Option<String> = None;

        loop {
            let mut builder = ScrollPointsBuilder::new(REALMS)
                .limit(100)
                .with_payload(true);
            if let Some(ref off) = offset {
                builder = builder.offset(off.clone());
            }

            let resp = self.client.scroll(builder).await?;
            for point in resp.result {
                let pid = point_id_to_string(point.id.as_ref());
                let map = map_to_json(&point.payload);
                if let Some(realm) = realm_from_payload(&pid, &map) {
                    realms.push(realm);
                }
            }

            if resp.next_page_offset.is_none() {
                break;
            }
            offset = resp.next_page_offset.map(|p| point_id_to_string(Some(&p)));
        }

        Ok(realms)
    }

    pub async fn get_realm(&self, id: &str) -> anyhow::Result<Option<RealmPoint>> {
        let resp = self
            .client
            .get_points(
                GetPointsBuilder::new(REALMS, vec![id.into()])
                    .with_payload(true)
                    .with_vectors(false),
            )
            .await?;

        if let Some(point) = resp.result.into_iter().next() {
            let pid = point_id_to_string(point.id.as_ref());
            let map = map_to_json(&point.payload);
            return Ok(realm_from_payload(&pid, &map));
        }
        Ok(None)
    }

    pub async fn find_realm_by_name(&self, name: &str) -> anyhow::Result<Option<RealmPoint>> {
        let filter = Filter::must([Condition::matches("name", name.to_string())]);
        let resp = self
            .client
            .scroll(
                ScrollPointsBuilder::new(REALMS)
                    .filter(filter)
                    .limit(1)
                    .with_payload(true),
            )
            .await?;

        if let Some(point) = resp.result.into_iter().next() {
            let pid = point_id_to_string(point.id.as_ref());
            let map = map_to_json(&point.payload);
            return Ok(realm_from_payload(&pid, &map));
        }
        Ok(None)
    }

    pub async fn store_realm(
        &self,
        id: &str,
        centroid: &[f32],
        name: &str,
        description: Option<&str>,
        is_user_pinned: bool,
    ) -> anyhow::Result<()> {
        let count = self.count_memories_in_realm(id).await?;
        let realm = RealmPoint {
            id: id.to_string(),
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            memory_count: count,
            is_user_pinned,
            centroid: centroid.to_vec(),
        };

        if let Some(existing) = self.find_realm_by_name(name).await? {
            let centroid_arr: Vec<serde_json::Value> =
                centroid.iter().map(|v| serde_json::json!(v)).collect();
            let payload: Payload = serde_json::json!({
                "memory_count": count,
                "is_user_pinned": is_user_pinned,
                "centroid": centroid_arr,
            })
            .try_into()
            .unwrap_or_default();
            self.client
                .set_payload(
                    SetPayloadPointsBuilder::new(REALMS, payload)
                        .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                            ids: vec![existing.id.into()],
                        }))
                        .wait(true),
                )
                .await?;
            return Ok(());
        }

        let payload = realm_to_payload(&realm);
        let point = PointStruct::new(id.to_string(), centroid.to_vec(), payload);
        self.client
            .upsert_points(UpsertPointsBuilder::new(REALMS, vec![point]).wait(true))
            .await?;
        Ok(())
    }

    pub async fn delete_realm(&self, id: &str) -> anyhow::Result<()> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(REALMS)
                    .points(PointsIdsList {
                        ids: vec![id.into()],
                    })
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    async fn count_memories_in_realm(&self, realm_id: &str) -> anyhow::Result<u32> {
        let filter = Filter::must([Condition::matches("realm_id", realm_id.to_string())]);
        let resp = self
            .client
            .count(CountPointsBuilder::new(MEMORIES).filter(filter))
            .await?;
        Ok(resp.result.map(|r| r.count).unwrap_or(0) as u32)
    }

    pub async fn update_realm_counts(&self) -> anyhow::Result<()> {
        let realms = self.list_realms().await?;
        for realm in realms {
            let count = self.count_memories_in_realm(&realm.id).await?;
            let payload: Payload = serde_json::json!({ "memory_count": count })
                .try_into()
                .unwrap_or_default();
            self.client
                .set_payload(
                    SetPayloadPointsBuilder::new(REALMS, payload)
                        .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                            ids: vec![realm.id.into()],
                        }))
                        .wait(true),
                )
                .await?;
        }
        Ok(())
    }

    /// Update the name of a realm.
    pub async fn update_realm_name(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let payload: Payload = serde_json::json!({ "name": name })
            .try_into()
            .unwrap_or_default();
        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(REALMS, payload)
                    .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: vec![id.into()],
                    }))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    // ── quantized (slumber) ───────────────────────────────────────────────────

    pub async fn store_quantized(
        &self,
        id: &str,
        vector: &[f32],
        payload: &MemoryPoint,
    ) -> anyhow::Result<()> {
        let point = PointStruct::new(id.to_string(), vector.to_vec(), memory_to_payload(payload));
        self.client
            .upsert_points(UpsertPointsBuilder::new(QUANTIZED, vec![point]).wait(true))
            .await?;
        Ok(())
    }

    // ── stats ─────────────────────────────────────────────────────────────────

    pub async fn get_collection_stats(&self, collection: &str) -> anyhow::Result<CollectionStats> {
        let info = self.client.collection_info(collection).await?;
        let vectors_count = info
            .result
            .map(|r| r.points_count)
            .unwrap_or(Some(0))
            .unwrap_or(0);

        Ok(CollectionStats {
            vector_count: vectors_count,
            size_bytes: 0,
        })
    }

    pub fn clone_store(&self) -> Self {
        self.clone()
    }

    /// Update arbitrary payload fields on a memory point.
    pub async fn update_memory_payload(&self, id: &str, payload: Payload) -> anyhow::Result<()> {
        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(MEMORIES, payload)
                    .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: vec![id.into()],
                    }))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    pub async fn count_realms(&self) -> anyhow::Result<u64> {
        let resp = self.client.count(CountPointsBuilder::new(REALMS)).await?;
        Ok(resp.result.map(|r| r.count as u64).unwrap_or(0))
    }

    /// Compute the mean vector (centroid) for all memories in a realm.
    pub async fn compute_realm_centroid(&self, realm_id: &str) -> anyhow::Result<Option<Vec<f32>>> {
        let all = self.scroll_all_memories_with_vectors().await?;
        let realm_memories: Vec<_> = all
            .into_iter()
            .filter(|m| m.memory.realm_id.as_deref() == Some(realm_id))
            .collect();

        if realm_memories.is_empty() {
            return Ok(None);
        }

        let dims = realm_memories[0].vector.len();
        let mut centroid = vec![0.0f32; dims];
        for mem in &realm_memories {
            for (i, &v) in mem.vector.iter().enumerate() {
                centroid[i] += v;
            }
        }
        for x in centroid.iter_mut() {
            *x /= realm_memories.len() as f32;
        }

        Ok(Some(centroid))
    }

    /// Recompute centroids for all realms and update their vectors.
    pub async fn recompute_all_realm_centroids(&self) -> anyhow::Result<usize> {
        let realms = self.list_realms().await?;
        let mut updated = 0;

        for realm in &realms {
            if let Some(centroid) = self.compute_realm_centroid(&realm.id).await? {
                // Update the realm's vector AND centroid payload in Qdrant
                let centroid_arr: Vec<serde_json::Value> =
                    centroid.iter().map(|v| serde_json::json!(v)).collect();
                let payload: Payload = serde_json::json!({
                    "name": realm.name,
                    "memory_count": realm.memory_count,
                    "is_user_pinned": realm.is_user_pinned,
                    "description": realm.description,
                    "centroid": centroid_arr,
                })
                .try_into()
                .unwrap_or_default();

                let point =
                    qdrant_client::qdrant::PointStruct::new(realm.id.clone(), centroid, payload);
                self.client
                    .upsert_points(UpsertPointsBuilder::new(REALMS, vec![point]).wait(true))
                    .await?;
                updated += 1;
            }
        }

        Ok(updated)
    }

    /// Update optimizer config for a collection (triggers background optimization).
    pub async fn update_collection_optimizer(
        &self,
        collection_name: &str,
        config: qdrant_client::qdrant::OptimizersConfigDiff,
    ) -> anyhow::Result<()> {
        self.client
            .update_collection(
                qdrant_client::qdrant::UpdateCollectionBuilder::new(collection_name.to_string())
                    .optimizers_config(config)
                    .build(),
            )
            .await?;
        Ok(())
    }

    /// Batch update payload on multiple memory points.
    /// Each entry is (point_id, payload) — updates are sent without wait (fire-and-forget).
    pub async fn batch_update_payload(
        &self,
        ids_and_payloads: &[(&str, Payload)],
    ) -> anyhow::Result<()> {
        if ids_and_payloads.is_empty() {
            return Ok(());
        }

        for (id, payload) in ids_and_payloads {
            self.client
                .set_payload(
                    SetPayloadPointsBuilder::new(MEMORIES, payload.clone())
                        .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                            ids: vec![(*id).into()],
                        }))
                        .wait(false),
                )
                .await?;
        }

        Ok(())
    }

    /// Find top-K nearest neighbors for a given memory by vector similarity.
    /// Returns vec of (memory_id, cosine_similarity).
    /// The memory_id itself is excluded from results.
    pub async fn find_similar(
        &self,
        memory_id: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        // Get the vector for this memory
        let resp = self
            .client
            .get_points(
                GetPointsBuilder::new(MEMORIES, vec![memory_id.into()])
                    .with_payload(false)
                    .with_vectors(true),
            )
            .await?;

        let vector = resp
            .result
            .into_iter()
            .next()
            .and_then(|p| extract_vector(&p));

        let Some(vector) = vector else {
            return Ok(vec![]);
        };

        // Search for similar vectors (ask for top_k + 1 to account for self)
        let resp = self
            .client
            .search_points(
                SearchPointsBuilder::new(MEMORIES, vector.as_slice(), (top_k + 1) as u64)
                    .with_payload(false)
                    .with_vectors(false),
            )
            .await?;

        let results: Vec<(String, f32)> = resp
            .result
            .into_iter()
            .filter(|r| point_id_to_string(r.id.as_ref()) != memory_id)
            .map(|r| (point_id_to_string(r.id.as_ref()), r.score))
            .take(top_k)
            .collect();

        Ok(results)
    }
}
