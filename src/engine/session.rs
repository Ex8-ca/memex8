//! Session-End Extraction — Phase 5 of Proactive Memory Inference
//!
//! Triggered when a conversation/webhook session ends. Uses LLM-assisted extraction
//! to identify:
//!   - **Decisions made**: conclusions, agreements, resolved questions
//!   - **Follow-ups requested**: todos, action items, future steps
//!   - **Key insights**: non-obvious learnings or discoveries
//!
//! Session summaries are stored as high-importance memories in a special realm
//! so they survive decay and are easily searchable.

use crate::config::AppConfig;
use crate::storage::qdrant::QdrantStore;
use serde::{Deserialize, Serialize};

/// A single extracted item from a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedItem {
    /// Type: "decision", "follow_up", or "insight"
    pub item_type: String,
    /// The extracted text.
    pub content: String,
    /// Which message index this was extracted from (for provenance).
    pub source_message_idx: Option<usize>,
    /// Confidence score from the LLM (0.0–1.0).
    pub confidence: f32,
}

/// A complete session extraction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionExtraction {
    pub session_id: String,
    /// Brief one-sentence summary of the overall session.
    pub summary: String,
    /// Decisions made during the session.
    pub decisions: Vec<ExtractedItem>,
    /// Follow-ups / action items requested.
    pub follow_ups: Vec<ExtractedItem>,
    /// Key insights or non-obvious learnings.
    pub insights: Vec<ExtractedItem>,
    /// Raw LLM response (for debugging/audit).
    #[serde(default)]
    pub raw_response: String,
}

/// Input for session-end extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInput {
    /// Unique session identifier.
    pub session_id: String,
    /// User or agent that participated in the session.
    pub user_id: Option<String>,
    /// Platform the session came from (e.g. "telegram", "openclaw", "hermes").
    pub platform: Option<String>,
    /// Conversation messages in order. Each message has role + content.
    pub messages: Vec<SessionMessage>,
    /// Optional pre-provided summary (e.g. from the webhook caller).
    pub provided_summary: Option<String>,
    /// Optional realm hint for where to store the session summary memory.
    pub realm_hint: Option<String>,
}

/// A single message in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    /// "user", "assistant", "system", etc.
    pub role: String,
    pub content: String,
}

/// Report from storing a session extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReport {
    pub session_id: String,
    pub summary_memory_id: Option<String>,
    pub decisions_stored: usize,
    pub follow_ups_stored: usize,
    pub insights_stored: usize,
    pub realm_name: String,
}

// ─── Extraction Engine ────────────────────────────────────────────────────────

pub struct SessionEngine {
    config: AppConfig,
    store: QdrantStore,
}

impl SessionEngine {
    pub fn new(config: AppConfig, store: QdrantStore) -> Self {
        Self { config, store }
    }

    /// Run the full session-end extraction pipeline:
    /// 1. LLM-assisted extraction of decisions, follow-ups, and insights
    /// 2. Store session summary as a high-importance memory
    /// 3. Store each extracted item as an individual memory
    pub async fn run_session_end(
        &self,
        input: SessionInput,
    ) -> anyhow::Result<SessionReport> {
        // Step 1: LLM-assisted extraction
        let extraction = self.extract(&input).await?;

        // Step 2: Store session summary as high-importance memory
        let realm_hint = input.realm_hint.as_deref().unwrap_or("sessions");
        let (realm_id, realm_name) = self.resolve_or_create_realm(realm_hint).await?;

        let summary_memory_id = self
            .store_session_summary(&extraction, &input, &realm_id, &realm_name)
            .await?;

        // Step 3: Store individual extracted items
        let decisions_stored = self
            .store_extracted_items(&extraction.decisions, "decision", &realm_id, &realm_name, &input)
            .await?;

        let follow_ups_stored = self
            .store_extracted_items(&extraction.follow_ups, "follow_up", &realm_id, &realm_name, &input)
            .await?;

        let insights_stored = self
            .store_extracted_items(&extraction.insights, "insight", &realm_id, &realm_name, &input)
            .await?;

        tracing::info!(
            "📋 Session {} extracted: {} decisions, {} follow-ups, {} insights",
            extraction.session_id,
            decisions_stored,
            follow_ups_stored,
            insights_stored
        );

        Ok(SessionReport {
            session_id: extraction.session_id,
            summary_memory_id,
            decisions_stored,
            follow_ups_stored,
            insights_stored,
            realm_name,
        })
    }

    /// Call LLM to extract decisions, follow-ups, and insights from conversation messages.
    async fn extract(&self, input: &SessionInput) -> anyhow::Result<SessionExtraction> {
        let llm_url =
            std::env::var("LOCAL_LLM_URL").unwrap_or_else(|_| "http://192.168.1.8:8888".into());
        let llm_key = std::env::var("LOCAL_LLM_API_KEY").ok();

        // Build message context (last N messages to stay within context limits)
        let messages_json: Vec<String> = input
            .messages
            .iter()
            .rev()
            .take(50) // last 50 messages max
            .rev() // restore original order
            .enumerate()
            .map(|(i, m)| format!("[{}] {}: {}", i, m.role, m.content))
            .collect();

        let messages_text = messages_json.join("\n");

        let prompt = format!(
            r#"You are an AI memory analyst. Analyze the following conversation and extract:

1. **DECISIONS**: Explicit conclusions, agreements, resolved questions, or commitments made during the session. Format: one per line starting with "- DECISION: "
2. **FOLLOW-UPS**: Action items, todos, things to do later, questions that were raised but not answered. Format: one per line starting with "- FOLLOW_UP: "
3. **INSIGHTS**: Non-obvious learnings, discoveries, or interesting observations. Format: one per line starting with "- INSIGHT: "
4. **SUMMARY**: A brief 1-2 sentence summary of the overall session.

Conversation:
{messages_text}

Output your analysis in this exact JSON format:
{{
  "decisions": [
    {{"content": "...", "source_message_idx": N, "confidence": 0.0-1.0}},
    ...
  ],
  "follow_ups": [
    {{"content": "...", "source_message_idx": N, "confidence": 0.0-1.0}},
    ...
  ],
  "insights": [
    {{"content": "...", "source_message_idx": N, "confidence": 0.0-1.0}},
    ...
  ],
  "summary": "Brief session summary..."
}}

Only output valid JSON. Be precise and extract only things explicitly present in the conversation."#,
            messages_text = messages_text
        );

        let raw_response = self
            .call_llm(&llm_url, llm_key.as_deref(), &prompt)
            .await?;

        // Parse JSON from LLM response
        let extraction = match serde_json::from_str::<SessionExtraction>(&raw_response) {
            Ok(e) => e,
            Err(_) => {
                // Try to extract JSON block from response
                if let Some(start) = raw_response.find('{') {
                    if let Some(end) = raw_response.rfind('}') {
                        let json_str = &raw_response[start..=end];
                        serde_json::from_str(json_str).unwrap_or_else(|e| {
                            tracing::warn!("Failed to parse LLM session extraction: {}", e);
                            SessionExtraction {
                                session_id: input.session_id.clone(),
                                summary: input
                                    .provided_summary
                                    .clone()
                                    .unwrap_or_else(|| "Session ended".to_string()),
                                decisions: vec![],
                                follow_ups: vec![],
                                insights: vec![],
                                raw_response: raw_response.clone(),
                            }
                        })
                    } else {
                        SessionExtraction {
                            session_id: input.session_id.clone(),
                            summary: input
                                .provided_summary
                                .clone()
                                .unwrap_or_else(|| "Session ended".to_string()),
                            decisions: vec![],
                            follow_ups: vec![],
                            insights: vec![],
                            raw_response: raw_response.clone(),
                        }
                    }
                } else {
                    SessionExtraction {
                        session_id: input.session_id.clone(),
                        summary: input
                            .provided_summary
                            .clone()
                            .unwrap_or_else(|| "Session ended".to_string()),
                        decisions: vec![],
                        follow_ups: vec![],
                        insights: vec![],
                        raw_response: raw_response.clone(),
                    }
                }
            }
        };

        Ok(extraction)
    }

    /// Call the local LLM with a prompt.
    async fn call_llm(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let client = reqwest::Client::new();

        let mut request_body = serde_json::json!({
            "model": "auto",
            "prompt": prompt,
            "stream": false,
        });

        if let Some(key) = api_key {
            request_body["api_key"] = serde_json::json!(key);
        }

        let mut request = client
            .post(format!("{}/generate", base_url))
            .header("Content-Type", "application/json")
            .json(&request_body);

        if let Some(key) = api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!("LLM request failed ({}): {}", status, body);
            return Err(anyhow::anyhow!("LLM request failed: {} - {}", status, body));
        }

        #[derive(Deserialize)]
        struct LlmResponse {
            response: Option<String>,
            #[serde(default)]
            output: Option<String>,
            #[serde(default)]
            content: Option<String>,
        }

        let llm_resp: LlmResponse = response.json().await?;

        let text = llm_resp
            .response
            .or(llm_resp.output)
            .or(llm_resp.content)
            .ok_or_else(|| anyhow::anyhow!("LLM returned no text"))?;

        Ok(text)
    }

    /// Resolve a realm by name, or create it if it doesn't exist.
    async fn resolve_or_create_realm(
        &self,
        name: &str,
    ) -> anyhow::Result<(String, String)> {
        // Try to find existing realm
        if let Some(realm) = self.store.find_realm_by_name(name).await? {
            return Ok((realm.id, realm.name));
        }

        // Create new realm with zero centroid (will be updated during next slumber)
        let id = uuid::Uuid::new_v4().to_string();
        let dimensions = self.config.embedding.dimensions as usize;
        let centroid = vec![0.0; dimensions];
        self.store
            .store_realm(&id, &centroid, name, None, false)
            .await?;

        Ok((id, name.to_string()))
    }

    /// Store the session summary as a high-importance memory.
    async fn store_session_summary(
        &self,
        extraction: &SessionExtraction,
        input: &SessionInput,
        realm_id: &str,
        realm_name: &str,
    ) -> anyhow::Result<Option<String>> {
        // Build full summary content
        let mut content = format!("# Session Summary: {}\n\n", extraction.session_id);
        content.push_str(&format!("**Summary:** {}\n\n", extraction.summary));

        if !extraction.decisions.is_empty() {
            content.push_str("## Decisions Made\n\n");
            for item in &extraction.decisions {
                content.push_str(&format!("- {}\n", item.content));
            }
            content.push('\n');
        }

        if !extraction.follow_ups.is_empty() {
            content.push_str("## Follow-Ups / Action Items\n\n");
            for item in &extraction.follow_ups {
                content.push_str(&format!("- {}\n", item.content));
            }
            content.push('\n');
        }

        if !extraction.insights.is_empty() {
            content.push_str("## Key Insights\n\n");
            for item in &extraction.insights {
                content.push_str(&format!("- {}\n", item.content));
            }
            content.push('\n');
        }

        if let Some(ref platform) = input.platform {
            content.push_str(&format!("**Platform:** {}\n", platform));
        }
        if let Some(ref user_id) = input.user_id {
            content.push_str(&format!("**User:** {}\n", user_id));
        }
        content.push_str(&format!("**Messages:** {}\n", input.messages.len()));

        // Generate embedding for the summary
        let embedder = crate::engine::embedder::create_embedder(&self.config)?;
        let vector = embedder.embed(&content).await?;

        let memory_id = uuid::Uuid::new_v4().to_string();
        let reaction_score = crate::engine::reactions::infer_reaction(&content);
        // Session summaries are high importance by default (1.5 initial boost)
        let importance = 1.5f32;

        self.store
            .store_memory(
                &memory_id,
                &vector,
                &content,
                None,
                Some(&format!("session:{}", input.session_id)),
                realm_id,
                realm_name,
                "",
                "session_summary",
                "session_summary",
                reaction_score,
            )
            .await?;

        // Update importance separately (store_memory normalizes it)
        // Re-store with higher importance by updating the memory's importance field
        let _ = self.store.set_memory_importance(&memory_id, importance).await;

        tracing::info!(
            "📋 Stored session summary memory (id: {}, realm: {})",
            memory_id,
            realm_name
        );

        Ok(Some(memory_id))
    }

    /// Store individual extracted items (decisions, follow-ups, insights) as memories.
    async fn store_extracted_items(
        &self,
        items: &[ExtractedItem],
        memory_type: &str,
        realm_id: &str,
        realm_name: &str,
        input: &SessionInput,
    ) -> anyhow::Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }

        let embedder = crate::engine::embedder::create_embedder(&self.config)?;

        let mut stored = 0;
        for item in items {
            // Skip low-confidence items
            if item.confidence < 0.5 {
                continue;
            }

            let content = format!(
                "[{}] {}",
                item.item_type.to_uppercase(),
                item.content
            );

            let vector = embedder.embed(&content).await?;
            let memory_id = uuid::Uuid::new_v4().to_string();
            let reaction_score = crate::engine::reactions::infer_reaction(&content);
            // Extracted items get elevated importance
            let importance = 1.2f32;

            self.store
                .store_memory(
                    &memory_id,
                    &vector,
                    &content,
                    None,
                    Some(&format!(
                        "session:{}:{}",
                        input.session_id, item.item_type
                    )),
                    realm_id,
                    realm_name,
                    "",
                    memory_type,
                    memory_type,
                    reaction_score,
                )
                .await?;

                let _ = self.store.set_memory_importance(&memory_id, importance).await;
            stored += 1;
        }

        Ok(stored)
    }
}

// ─── Utilities ────────────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}
