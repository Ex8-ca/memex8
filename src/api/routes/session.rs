use crate::api::error::ApiError;
use crate::api::server::AppState;
use crate::engine::session::{SessionInput, SessionMessage};
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

/// POST /api/v1/sessions/end — Run session-end extraction on a conversation.
/// Extracts decisions, follow-ups, and insights using LLM assistance and stores
/// them as high-importance memories in the sessions realm.
pub async fn session_end(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SessionEndPayload>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let input = SessionInput {
        session_id: payload.session_id,
        user_id: payload.user_id,
        platform: payload.platform,
        messages: payload
            .messages
            .into_iter()
            .map(|m| SessionMessage {
                role: m.role,
                content: m.content,
            })
            .collect(),
        provided_summary: payload.summary,
        realm_hint: payload.realm_hint,
    };

    let report = state.engine.run_session_end(input).await?;

    Ok(Json(serde_json::json!({
        "status": "extracted",
        "session_id": report.session_id,
        "summary_memory_id": report.summary_memory_id,
        "decisions_stored": report.decisions_stored,
        "follow_ups_stored": report.follow_ups_stored,
        "insights_stored": report.insights_stored,
        "realm": report.realm_name,
    })))
}

#[derive(Deserialize)]
pub struct SessionEndPayload {
    /// Unique session identifier.
    pub session_id: String,
    /// User or agent that participated in the session.
    pub user_id: Option<String>,
    /// Platform the session came from (e.g. "telegram", "openclaw", "hermes").
    pub platform: Option<String>,
    /// Conversation messages in order.
    pub messages: Vec<MessagePayload>,
    /// Optional pre-provided summary.
    pub summary: Option<String>,
    /// Optional realm hint (defaults to "sessions").
    pub realm_hint: Option<String>,
}

#[derive(Deserialize)]
pub struct MessagePayload {
    pub role: String,
    pub content: String,
}
