//! Shared retrieval projections and ordered batch retrieval.
use super::{Engine, MemoryResult};
use crate::storage::qdrant::MemoryPoint;
use serde::Serialize;

pub const MAX_BATCH_IDS: usize = 50;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CompactMemoryResult {
    pub id: String,
    pub snippet: String,
    pub realm_name: String,
    pub score: f32,
    pub ingested_at: String,
    pub memory_type: String,
    pub verification_status: String,
}

impl MemoryResult {
    pub fn into_compact(self) -> CompactMemoryResult {
        let text = self
            .summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.content);
        CompactMemoryResult {
            id: self.id,
            snippet: text.chars().take(160).collect(),
            realm_name: self.realm_name,
            score: self.score,
            ingested_at: self.ingested_at,
            memory_type: self.memory_type,
            verification_status: self.verification_status,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RetrievalResults {
    Full(Vec<MemoryResult>),
    Compact(Vec<CompactMemoryResult>),
}

impl RetrievalResults {
    pub fn new(results: Vec<MemoryResult>, compact: bool) -> Self {
        if compact {
            Self::Compact(
                results
                    .into_iter()
                    .map(MemoryResult::into_compact)
                    .collect(),
            )
        } else {
            Self::Full(results)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BatchGetResult {
    pub memories: Vec<MemoryPoint>,
    pub not_found: Vec<String>,
}

pub fn validate_batch_ids(ids: &[String]) -> Result<(), &'static str> {
    if ids.is_empty() || ids.len() > MAX_BATCH_IDS {
        return Err("ids must contain between 1 and 50 memory IDs");
    }
    Ok(())
}

impl Engine {
    pub async fn batch_get(&self, ids: &[String]) -> anyhow::Result<BatchGetResult> {
        validate_batch_ids(ids).map_err(anyhow::Error::msg)?;
        self.store.get_memories(ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(content: &str, summary: Option<&str>) -> MemoryResult {
        MemoryResult {
            id: "id".into(),
            content: content.into(),
            summary: summary.map(Into::into),
            ingested_at: "2026-11-08T00:00:00Z".into(),
            memory_type: "fact".into(),
            verification_status: "verified".into(),
            heading: None,
            realm_name: "realm".into(),
            importance: 0.5,
            score: 0.8,
            last_accessed: String::new(),
            access_count: 0,
            upvotes: 0,
            related_memory_ids: vec![],
            association_strengths: vec![],
        }
    }

    #[test]
    fn snippets_bound_unicode_and_prefer_nonempty_summary() {
        let text = "🦀界é".repeat(100);
        let compact = result("body", Some(&text)).into_compact();
        assert_eq!(compact.snippet, text.chars().take(160).collect::<String>());
        assert_eq!(compact.snippet.chars().count(), 160);
        for empty in [None, Some(""), Some("  \n")] {
            assert_eq!(result(&text, empty).into_compact().snippet, compact.snippet);
        }
        assert_eq!(
            result("body", Some("summary")).into_compact().snippet,
            "summary"
        );
    }

    #[test]
    fn projections_keep_full_shape_and_compact_metadata_only() {
        let full = serde_json::to_value(result("body", Some("summary"))).unwrap();
        assert!(full.get("content").is_some());
        for field in [
            "summary",
            "ingested_at",
            "memory_type",
            "verification_status",
            "snippet",
        ] {
            assert!(full.get(field).is_none());
        }
        let compact = serde_json::to_value(result("body", None).into_compact()).unwrap();
        assert_eq!(compact.as_object().unwrap().len(), 7);
        assert_eq!(compact["verification_status"], "verified");
        assert_eq!(compact["ingested_at"], "2026-11-08T00:00:00Z");
        assert_eq!(compact["snippet"], "body");
    }
}
