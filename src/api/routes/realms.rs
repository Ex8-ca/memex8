use crate::api::server::AppState;
use crate::storage::qdrant::RealmPoint;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct CreateRealmRequest {
    pub name: String,
    pub description: Option<String>,
}

/// Public view of a realm. Omits the centroid embedding: it is an internal
/// clustering detail, and at 1536 floats per realm it made `GET /realms`
/// ~250 KB for a dozen realms — enough to blow out an agent's tool budget.
/// Backups serialize `RealmPoint` directly and keep the centroid.
#[derive(Debug, Serialize)]
pub struct RealmSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub memory_count: u32,
    pub is_user_pinned: bool,
}

impl From<RealmPoint> for RealmSummary {
    fn from(r: RealmPoint) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            memory_count: r.memory_count,
            is_user_pinned: r.is_user_pinned,
        }
    }
}

pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RealmSummary>>, crate::api::error::ApiError> {
    let realms = state.engine.list_realms().await?;
    Ok(Json(realms.into_iter().map(RealmSummary::from).collect()))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRealmRequest>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    state
        .engine
        .create_realm(&req.name, req.description.as_deref())
        .await?;
    Ok(Json(serde_json::json!({"created": req.name})))
}

pub async fn show(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<RealmSummary>, crate::api::error::ApiError> {
    let realm = state.engine.show_realm(&name).await?;
    Ok(Json(realm.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_omits_centroid() {
        let point = RealmPoint {
            id: "r1".into(),
            name: "Realm".into(),
            description: None,
            centroid: vec![0.5; 1536],
            memory_count: 7,
            is_user_pinned: false,
        };
        let json = serde_json::to_value(RealmSummary::from(point)).unwrap();
        assert!(json.get("centroid").is_none());
        assert_eq!(json["memory_count"], 7);
        assert_eq!(json["name"], "Realm");
    }
}
