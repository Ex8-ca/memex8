use crate::api::error::ApiError;
use crate::api::server::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct StoreRequest {
    pub content: String,
    pub tags: Option<Vec<String>>,
    pub realm_hint: Option<String>,
    pub source: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub realm: Option<String>,
    pub tags: Option<Vec<String>>,
    pub min_score: Option<f32>,
}

#[derive(Deserialize)]
pub struct IngestRequest {
    pub path: String,
    pub chunk_by: Option<String>,
    pub realm_hint: Option<String>,
}

#[derive(Serialize)]
pub struct StoreResponse {
    pub id: String,
    pub status: String,
}

pub async fn store(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StoreRequest>,
) -> Result<Json<StoreResponse>, crate::api::error::ApiError> {
    let id = state
        .engine
        .store_memory(
            &req.content,
            req.tags,
            req.realm_hint.as_deref(),
            req.source.as_deref(),
        )
        .await?;
    Ok(Json(StoreResponse {
        id,
        status: "stored".into(),
    }))
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, crate::api::error::ApiError> {
    let limit = req.limit.unwrap_or(10);
    let offset = req.offset.unwrap_or(0);
    let tags_ref = req.tags.clone();
    let results = state
        .engine
        .search(
            &req.query,
            req.realm.as_deref(),
            tags_ref.as_deref().map(|t| t.as_ref()),
            limit,
            offset,
            req.min_score.unwrap_or(0.3),
        )
        .await?;
    let total = results.len();
    Ok(Json(SearchResponse {
        results,
        total,
        limit,
        offset,
    }))
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub results: Vec<crate::engine::MemoryResult>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<crate::storage::qdrant::MemoryPoint>, crate::api::error::ApiError> {
    let memory = state.engine.get_memory(&id).await?;
    Ok(Json(memory))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.delete_memory(&id).await?;
    Ok(Json(serde_json::json!({"deleted": id})))
}

pub async fn recall(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RecallParams>,
) -> Result<Json<RecallResponse>, crate::api::error::ApiError> {
    let limit = params.limit.unwrap_or(10);
    let offset = params.offset.unwrap_or(0);
    let all_results = state
        .engine
        .recall(limit + offset, params.realm.as_deref())
        .await?;
    let total = all_results.len();
    let results: Vec<_> = all_results.into_iter().skip(offset).take(limit).collect();
    Ok(Json(RecallResponse {
        results,
        total,
        limit,
        offset,
    }))
}

#[derive(Serialize)]
pub struct RecallResponse {
    pub results: Vec<crate::engine::MemoryResult>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Get tag suggestions (most commonly used tags).
pub async fn tags(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TagParams>,
) -> Result<Json<Vec<TagSuggestion>>, crate::api::error::ApiError> {
    let limit = params.limit.unwrap_or(20);
    let tags = state.engine.get_tag_suggestions(limit).await?;
    Ok(Json(
        tags.into_iter()
            .map(|(tag, count)| TagSuggestion { tag, count })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct TagParams {
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct TagSuggestion {
    pub tag: String,
    pub count: u32,
}

#[derive(Deserialize)]
pub struct RecallParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub realm: Option<String>,
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub realm: Option<String>,
    /// Sort field: "ingested_at", "importance", "last_accessed", "access_count". Default: "ingested_at"
    #[serde(default = "default_sort")]
    pub sort: String,
    /// Sort direction: "asc" or "desc". Default: "desc" (newest first)
    #[serde(default = "default_dir")]
    pub direction: String,
}

fn default_sort() -> String {
    "ingested_at".into()
}
fn default_dir() -> String {
    "desc".into()
}

/// GET /api/v1/memories — List all memories with optional sort/filter, no recency bias.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<ListResponse>, crate::api::error::ApiError> {
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);
    let _sort_field = params.sort.as_str();
    let _descending = params.direction.as_str() != "asc";

    let memories = state
        .engine
        .list_memories(params.realm.as_deref(), &params.sort, params.direction.as_str() != "asc")
        .await?;
    let total = memories.len();
    let page: Vec<_> = memories.into_iter().skip(offset).take(limit).collect();

    Ok(Json(ListResponse { memories: page, total, limit, offset }))
}

#[derive(Serialize)]
pub struct ListResponse {
    pub memories: Vec<crate::storage::qdrant::MemoryPoint>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// GET /api/v1/memories/verification-summary — counts by verification status.
/// Registered before `/memories/{id}` so the static segment wins.
pub async fn verification_summary(
    State(state): State<Arc<AppState>>,
) -> Result<Json<crate::storage::qdrant::VerificationStatusCounts>, crate::api::error::ApiError> {
    let counts = state.engine.verification_summary().await?;
    Ok(Json(counts))
}

pub async fn ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state
        .engine
        .ingest_path(
            &req.path,
            req.chunk_by.as_deref().unwrap_or("section"),
            req.realm_hint.as_deref(),
        )
        .await?;
    Ok(Json(
        serde_json::json!({"status": "ingested", "path": req.path}),
    ))
}

pub async fn upvote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.upvote(&id).await?;
    Ok(Json(serde_json::json!({ "status": "upvoted" })))
}

pub async fn downvote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.downvote(&id).await?;
    Ok(Json(serde_json::json!({ "status": "downvoted" })))
}

/// PATCH /api/v1/memories/{id} — partial update of payload fields.
/// Accepts any subset of {memory_type, importance}; applies via Qdrant set_payload.
pub async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Accept any subset of payload fields; apply via set_payload.
    let mut payload_obj = serde_json::Map::new();
    if let Some(t) = req.get("memory_type").and_then(|v| v.as_str()) {
        payload_obj.insert(
            "memory_type".to_string(),
            serde_json::Value::String(t.to_string()),
        );
    }
    if let Some(i) = req.get("importance").and_then(|v| v.as_f64()) {
        payload_obj.insert(
            "importance".to_string(),
            serde_json::json!(i as f32),
        );
    }
    if payload_obj.is_empty() {
        return Ok(Json(serde_json::json!({"id": id, "status": "no-op"})));
    }
    let payload: qdrant_client::Payload = serde_json::Value::Object(payload_obj)
        .try_into()
        .unwrap_or_default();
    state
        .engine
        .update_memory_payload(&id, payload)
        .await?;
    Ok(Json(serde_json::json!({"id": id, "status": "updated"})))
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.archive_memory(&id).await?;
    Ok(Json(serde_json::json!({"archived": id})))
}
