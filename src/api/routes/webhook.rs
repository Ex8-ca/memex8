use crate::api::server::AppState;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Webhook endpoint for agent conversation ingestion.
/// Both OpenClaw and Hermes can POST here on conversation end.
pub async fn conversation_end(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebhookPayload>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    let source = payload.source.as_deref().unwrap_or("unknown");
    let platform = payload.platform.as_deref().unwrap_or("unknown");

    // Build memory content from conversation
    let mut content = format!("# Conversation Summary\n\n");
    if let Some(ref summary) = payload.summary {
        content.push_str(summary);
        content.push_str("\n\n");
    }
    if let Some(ref messages) = payload.messages {
        content.push_str("## Messages\n\n");
        for msg in messages {
            content.push_str(&format!("**{}**: {}\n\n", msg.role, msg.content));
        }
    }

    let tags = vec![
        "conversation".to_string(),
        format!("platform:{}", platform),
    ];

    let realm_hint = payload.realm_hint.clone();

    let id = state.engine.store_memory(
        &content,
        Some(tags),
        realm_hint.as_deref(),
        Some(source),
    ).await?;

    tracing::info!(
        "📥 Webhook: stored {} conversation (id: {})",
        platform, id
    );

    Ok(Json(serde_json::json!({
        "status": "stored",
        "id": id,
        "source": source,
        "platform": platform,
    })))
}

/// Webhook endpoint for skill execution results.
pub async fn skill_executed(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SkillPayload>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    let content = format!(
        "# Skill Execution: {}\n\n**Status**: {}\n**Input**: {}\n**Output**: {}",
        payload.skill_name,
        payload.status,
        serde_json::to_string_pretty(&payload.input).unwrap_or_default(),
        serde_json::to_string_pretty(&payload.output).unwrap_or_default(),
    );

    let tags = vec![
        "skill".to_string(),
        payload.skill_category.clone().unwrap_or_else(|| "general".to_string()),
    ];

    let id = state.engine.store_memory(
        &content,
        Some(tags),
        payload.realm_hint.as_deref(),
        Some("openclaw"),
    ).await?;

    tracing::info!(
        "📥 Webhook: stored skill execution (id: {}, skill: {})",
        id, payload.skill_name
    );

    Ok(Json(serde_json::json!({
        "status": "stored",
        "id": id,
        "skill": payload.skill_name,
    })))
}

// ─── Payload Types ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WebhookPayload {
    pub summary: Option<String>,
    pub messages: Option<Vec<Message>>,
    pub source: Option<String>,
    pub platform: Option<String>,
    pub realm_hint: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct SkillPayload {
    pub skill_name: String,
    pub skill_category: Option<String>,
    pub status: String,
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub realm_hint: Option<String>,
}
