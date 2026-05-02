use crate::api::server::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TriggerParams {
    #[serde(default)]
    pub force_consolidation: Option<bool>,
}

pub async fn status(State(state): State<Arc<AppState>>) -> Json<crate::engine::SlumberStatus> {
    Json(state.engine.slumber_status().await)
}

/// Trigger slumber. Optional `?force_consolidation=true` runs Phase 6 LLM consolidation
/// even outside the scheduled cron window.
pub async fn trigger(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TriggerParams>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    let force = params.force_consolidation.unwrap_or(false);
    state.engine.trigger_slumber(force).await?;
    Ok(Json(serde_json::json!({
        "status": "slumber_completed",
        "consolidation_forced": force,
    })))
}
