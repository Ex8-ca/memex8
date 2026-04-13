use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub content: String,
    pub source_ids: Vec<String>,
    pub confidence_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
}

pub struct Compressor;

impl Compressor {
    /// Summarize a cluster of related memories into a dense representation.
    ///
    /// Guardrails:
    /// - Preserve key decisions and reasoning
    /// - Preserve unique insights
    /// - Preserve entity names and relationships
    /// - Preserve actionable facts
    /// - Compute confidence score; flag if below threshold
    pub fn summarize_cluster(contents: &[String]) -> Summary {
        // TODO: use LLM or rule-based summarization
        // For now, concatenate first 200 chars of each
        let combined: Vec<String> = contents
            .iter()
            .map(|c| c.chars().take(200).collect::<String>())
            .collect();

        Summary {
            content: combined.join(" ... "),
            source_ids: vec![],
            confidence_score: 0.5,
        }
    }

    /// Extract named entities from text (rule-based)
    pub fn extract_entities(text: &str) -> Vec<Entity> {
        // TODO: implement NER — capitalize sequences, technical terms, etc.
        vec![]
    }

    /// Compute importance score for a memory based on multiple factors
    pub fn compute_importance(
        upvotes: u32,
        access_count: u32,
        age_days: f32,
        realm_centralness: f32,
    ) -> f32 {
        let vote_score = (upvotes as f32).ln_1p() / 5.0;
        let access_score = (access_count as f32).ln_1p() / 10.0;
        let recency_score = (-age_days / 30.0).exp();
        let central_score = realm_centralness;

        (vote_score + access_score + recency_score + central_score) / 4.0
    }
}
