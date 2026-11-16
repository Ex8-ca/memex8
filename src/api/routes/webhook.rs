use crate::api::server::AppState;
use crate::engine::redaction;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Webhook endpoint for agent conversation ingestion.
/// Both OpenClaw and Hermes can POST here on conversation end.
pub async fn conversation_end(
    State(state): State<Arc<AppState>>,
    Json(mut payload): Json<WebhookPayload>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    let mut parser = redaction::Redactor::default();
    let mut meaningful = false;
    if let Some(summary) = &mut payload.summary {
        *summary = parser.redact(summary);
        meaningful |= redaction::meaningful(summary);
    }
    if let Some(messages) = &mut payload.messages {
        for message in messages {
            message.content = parser.redact(&message.content);
            meaningful |= redaction::meaningful(&message.content);
        }
    }
    if !meaningful {
        return Err(anyhow::Error::from(redaction::RedactionError::Empty).into());
    }
    let payload: WebhookPayload = redaction::sanitize(&payload)?;
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

    // Callers that send no tags keep the historical defaults exactly.
    let mut tags = vec!["conversation".to_string(), format!("platform:{}", platform)];
    if let Some(extra) = &payload.tags {
        // Bounded: an archive's chunk tags ride along on every write, but a
        // caller should not be able to grow the payload without limit.
        for tag in extra.iter().take(MAX_EXTRA_TAGS) {
            let tag = tag.trim();
            if tag.is_empty() || tag.chars().count() > MAX_TAG_CHARS {
                continue;
            }
            if !tags.iter().any(|existing| existing == tag) {
                tags.push(tag.to_string());
            }
        }
    }

    // Optional caller-declared type. Unknown names fall back to `general` in
    // the decay table, so this cannot widen decay beyond what the server knows.
    let memory_type = payload
        .memory_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= MAX_MEMORY_TYPE_CHARS);

    let realm_hint = payload.realm_hint.clone();

    let id = state
        .engine
        .store_memory(
            &content,
            Some(tags),
            realm_hint.as_deref(),
            Some(source),
            // Webhook ingestion is for the agent's own records; never
            // share these with peers. Operators can mark individual
            // conversations public later via PATCH.
            Some("private"),
            // None keeps the pre-existing default ("general").
            memory_type,
        )
        .await?;

    tracing::info!("📥 Webhook: stored {} conversation (id: {})", platform, id);

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
    let payload: SkillPayload = redaction::sanitize(&payload)?;
    let content = format!(
        "# Skill Execution: {}\n\n**Status**: {}\n**Input**: {}\n**Output**: {}",
        payload.skill_name,
        payload.status,
        serde_json::to_string_pretty(&payload.input).unwrap_or_default(),
        serde_json::to_string_pretty(&payload.output).unwrap_or_default(),
    );

    let tags = vec![
        "skill".to_string(),
        payload
            .skill_category
            .clone()
            .unwrap_or_else(|| "general".to_string()),
    ];

    let id = state
        .engine
        .store_memory(
            &content,
            Some(tags),
            payload.realm_hint.as_deref(),
            Some("openclaw"),
            // Skill execution traces are operator-private by default.
            Some("private"),
            None, // memory_type: skill traces use the default "general"
        )
        .await?;

    tracing::info!(
        "📥 Webhook: stored skill execution (id: {}, skill: {})",
        id,
        payload.skill_name
    );

    Ok(Json(serde_json::json!({
        "status": "stored",
        "id": id,
        "skill": payload.skill_name,
    })))
}

// ─── Payload Types ──────────────────────────────────────────────

/// Caps on caller-supplied provenance so one webhook body stays bounded.
const MAX_EXTRA_TAGS: usize = 16;
const MAX_TAG_CHARS: usize = 64;
const MAX_MEMORY_TYPE_CHARS: usize = 32;

#[derive(Deserialize, Serialize)]
pub struct WebhookPayload {
    pub summary: Option<String>,
    pub messages: Option<Vec<Message>>,
    pub source: Option<String>,
    pub platform: Option<String>,
    pub realm_hint: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Optional memory_type (e.g. `artifact` for checkpoint archives). When
    /// absent the stored memory keeps the historical `general` default.
    pub memory_type: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Deserialize, Serialize)]
pub struct SkillPayload {
    pub skill_name: String,
    pub skill_category: Option<String>,
    pub status: String,
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub realm_hint: Option<String>,
}
