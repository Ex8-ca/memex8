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

pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.archive_memory(&id).await?;
    Ok(Json(serde_json::json!({"archived": id})))
}
