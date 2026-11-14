//! Dreams API. Dreams are not memories: they are served only from here, never from
//! search, recall, timeline or MCP.

use crate::api::error::ApiError;
use crate::api::server::AppState;
use crate::engine::dream::{Dream, DreamStatus};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<usize>,
    /// "pending" or "kept"; omit for all.
    pub status: Option<String>,
}

fn status_filter(s: Option<&str>) -> Result<Option<DreamStatus>, ApiError> {
    match s {
        None => Ok(None),
        Some("pending") => Ok(Some(DreamStatus::Pending)),
        Some("kept") => Ok(Some(DreamStatus::Kept)),
        Some(other) => Err(ApiError::BadRequest(format!(
            "status must be pending or kept, not '{other}'"
        ))),
    }
}

fn preview(d: &Dream) -> String {
    let flat = d.dream.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut p: String = flat.chars().take(160).collect();
    if flat.chars().count() > 160 {
        p.push('…');
    }
    p
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    let status = status_filter(params.status.as_deref())?;
    let limit = params.limit.unwrap_or(20).min(200);
    let store = state.engine.dream_store();
    let dreams: Vec<Value> = store
        .list(limit, status)
        .iter()
        .map(|d| {
            json!({
                "id": d.id, "created_at": d.created_at, "title": d.title,
                "preview": preview(d), "status": d.status,
            })
        })
        .collect();
    Ok(Json(json!({
        "dreams": dreams,
        "pending": store.pending_count(),
        "enabled": state.engine.dream_config().enabled,
    })))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Dream>, ApiError> {
    state
        .engine
        .dream_store()
        .get(&id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(id))
}

/// Dream now. Works even when the schedule is off; 409 if one is already running.
pub async fn create(State(state): State<Arc<AppState>>) -> Result<Json<Dream>, ApiError> {
    Ok(Json(state.engine.dream_now().await?))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if state.engine.dream_store().delete(&id)? {
        Ok(Json(json!({"deleted": id})))
    } else {
        Err(ApiError::NotFound(id))
    }
}

#[derive(Deserialize, Default)]
pub struct KeepBody {
    /// Also store the dream as a `dream`-type memory (hidden from normal search).
    #[serde(default)]
    pub remember: bool,
}

/// Mark a pending dream kept; optional body `{ "remember": true }`.
pub async fn keep(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<KeepBody>>,
) -> Result<Json<Dream>, ApiError> {
    let remember = body.map(|b| b.0.remember).unwrap_or(false);
    state
        .engine
        .keep_dream(&id, remember)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_filter_accepts_known_values_only() {
        assert_eq!(status_filter(None).unwrap(), None);
        assert_eq!(
            status_filter(Some("pending")).unwrap(),
            Some(DreamStatus::Pending)
        );
        assert_eq!(
            status_filter(Some("kept")).unwrap(),
            Some(DreamStatus::Kept)
        );
        assert!(status_filter(Some("all")).is_err());
    }

    #[test]
    fn preview_is_flat_and_short() {
        let d = Dream {
            id: "i".into(),
            created_at: "2026-11-13T00:00:00Z".into(),
            title: "t".into(),
            dream: format!("line one\n\n{}", "word ".repeat(100)),
            waking_note: "w".into(),
            waking_note_ids: vec![],
            seed_ids: vec![],
            model: "m".into(),
            status: DreamStatus::Pending,
            remembered_as: None,
        };
        let p = preview(&d);
        assert!(!p.contains('\n') && p.ends_with('…') && p.chars().count() <= 161);
    }
}
