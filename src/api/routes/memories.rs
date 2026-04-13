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
    pub realm: Option<String>,
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
    let id = state.engine.store_memory(
        &req.content,
        req.tags,
        req.realm_hint.as_deref(),
        req.source.as_deref(),
    ).await?;
    Ok(Json(StoreResponse {
        id,
        status: "stored".into(),
    }))
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Vec<crate::engine::MemoryResult>>, crate::api::error::ApiError> {
    let results = state.engine.search(
        &req.query,
        req.realm.as_deref(),
        req.limit.unwrap_or(10),
        req.min_score.unwrap_or(0.3),
    ).await?;
    Ok(Json(results))
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
) -> Result<Json<Vec<crate::engine::MemoryResult>>, crate::api::error::ApiError> {
    let results = state.engine.recall(
        params.limit.unwrap_or(10),
        params.realm.as_deref(),
    ).await?;
    Ok(Json(results))
}

#[derive(Deserialize)]
pub struct RecallParams {
    pub limit: Option<usize>,
    pub realm: Option<String>,
}

pub async fn ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.ingest_path(
        &req.path,
        req.chunk_by.as_deref().unwrap_or("section"),
        req.realm_hint.as_deref(),
    ).await?;
    Ok(Json(serde_json::json!({"status": "ingested", "path": req.path})))
}

pub async fn upvote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.upvote(&id).await?;
    Ok(Json(serde_json::json!({"upvoted": id})))
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.archive_memory(&id).await?;
    Ok(Json(serde_json::json!({"archived": id})))
}
