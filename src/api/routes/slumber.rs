use crate::api::server::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

pub async fn status(
    State(state): State<Arc<AppState>>,
) -> Json<crate::engine::SlumberStatus> {
    Json(state.engine.slumber_status().await)
}

pub async fn trigger(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.trigger_slumber().await?;
    Ok(Json(serde_json::json!({"status": "slumber_completed"})))
}
