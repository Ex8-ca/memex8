//! Inference API routes — Phase 3 of Proactive Memory Inference
//! Provides proactive suggestion and gap resolution endpoints.

use crate::api::error::ApiError;
use crate::api::server::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Request body for gap suggestion endpoint.
#[derive(Deserialize)]
pub struct SuggestRequest {
    /// Topic to analyze for gaps (mutually exclusive with memory_id).
    pub topic: Option<String>,
    /// Memory ID to analyze (mutually exclusive with topic).
    pub memory_id: Option<String>,
    /// Maximum number of suggestions to return.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    5
}

/// A single gap suggestion with metadata.
#[derive(Serialize)]
pub struct GapSuggestion {
    pub id: String,
    pub gap_type: String,
    pub suggested_topic: String,
    pub description: String,
    pub confidence: f32,
    pub related_memory_ids: Vec<String>,
    pub suggested_search_queries: Vec<String>,
    pub importance: f32,
    pub created_at: String,
}

/// Response for the suggest endpoint.
#[derive(Serialize)]
pub struct SuggestResponse {
    pub suggestions: Vec<GapSuggestion>,
}

/// GET /api/v1/inference/gaps — List all open gaps.
pub async fn list_gaps(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListGapsParams>,
) -> Result<Json<Vec<GapSuggestion>>, ApiError> {
    let status = if params.resolved.unwrap_or(false) {
        None // show all
    } else {
        Some("open")
    };

    let gaps = state.engine.list_gaps(status).await?;
    let suggestions: Vec<GapSuggestion> = gaps
        .into_iter()
        .map(|g| GapSuggestion {
            id: g.id,
            gap_type: g.gap_type,
            suggested_topic: g.suggested_topic,
            description: g.description,
            confidence: g.importance, // importance serves as confidence here
            related_memory_ids: g.related_memory_ids,
            suggested_search_queries: g.suggested_search_queries,
            importance: g.importance,
            created_at: g.created_at,
        })
        .collect();

    Ok(Json(suggestions))
}

#[derive(Deserialize)]
pub struct ListGapsParams {
    pub resolved: Option<bool>,
}

/// POST /api/v1/inference/suggest — Get proactive suggestions based on topic or memory.
pub async fn suggest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SuggestRequest>,
) -> Result<Json<SuggestResponse>, ApiError> {
    let suggestions = state
        .engine
        .infer_gaps(req.topic.as_deref(), req.memory_id.as_deref(), req.limit)
        .await?;

    Ok(Json(SuggestResponse { suggestions }))
}

/// POST /api/v1/inference/gaps/{id}/resolve — Mark a gap as resolved.
pub async fn resolve_gap(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.resolve_gap(&id).await?;
    Ok(Json(serde_json::json!({ "status": "resolved", "id": id })))
}

/// POST /api/v1/inference/gaps/{id}/dismiss — Dismiss a gap (mark as dismissed).
pub async fn dismiss_gap(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.dismiss_gap(&id).await?;
    Ok(Json(serde_json::json!({ "status": "dismissed", "id": id })))
}
