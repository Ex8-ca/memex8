use crate::api::server::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct CreateRealmRequest {
    pub name: String,
    pub description: Option<String>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<crate::storage::qdrant::RealmPoint>>, crate::api::error::ApiError> {
    let realms = state.engine.list_realms().await?;
    Ok(Json(realms))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRealmRequest>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state.engine.create_realm(&req.name, req.description.as_deref()).await?;
    Ok(Json(serde_json::json!({"created": req.name})))
}

pub async fn show(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<crate::storage::qdrant::RealmPoint>, crate::api::error::ApiError> {
    let realm = state.engine.show_realm(&name).await?;
    Ok(Json(realm))
}
