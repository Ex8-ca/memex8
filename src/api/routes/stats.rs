use crate::api::server::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

pub async fn stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<crate::engine::SystemStats>, crate::api::error::ApiError> {
    let stats = state.engine.stats().await?;
    Ok(Json(stats))
}
