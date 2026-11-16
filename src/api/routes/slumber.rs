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
/// even outside the scheduled cron window. Refused with 409 while paused.
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

/// Pause scheduled slumber.
///
/// The flag is persisted next to the compressed index, so it survives a restart
/// and the acknowledgement only comes back after the write succeeded. A pause
/// stops the scheduler's idle and cron triggers and refuses explicit triggers.
pub async fn pause(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.pause_slumber().await?;
    Ok(Json(status_body(&state, "slumber_paused").await))
}

/// Resume scheduled slumber (clears the persisted pause flag).
pub async fn resume(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.resume_slumber().await?;
    Ok(Json(status_body(&state, "slumber_resumed").await))
}

async fn status_body(state: &Arc<AppState>, action: &str) -> serde_json::Value {
    let status = state.engine.slumber_status().await;
    serde_json::json!({
        "status": action,
        "paused": status.paused,
        "slumber_state": status.state,
        "pause_file": state.engine.slumber_pause_path(),
    })
}
