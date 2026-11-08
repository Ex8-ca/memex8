//! Retrieval strength from a bounded raw-cosine probe, independent of ranking boosts.
use super::{Engine, MemoryResult};
use crate::config::CoverageConfig;
use serde::Serialize;

/// Candidate count is bounded and independent of result limit/offset.
pub const COVERAGE_CANDIDATES: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub struct SearchCoverage {
    pub level: &'static str,
    pub best_score: Option<f32>,
    pub results_above_min: usize,
    pub suggested_realms: Vec<String>,
    pub hint: String,
}

impl SearchCoverage {
    pub fn from_scores(scores: &[f32], min_score: f32, config: &CoverageConfig) -> Self {
        let best = scores
            .iter()
            .copied()
            .filter(|s| s.is_finite())
            .reduce(f32::max);
        let level = match best {
            Some(score) if score >= config.high => "high",
            Some(score) if score >= config.partial => "partial",
            _ => "none",
        };
        Self {
            level, best_score: best,
            results_above_min: scores.iter().filter(|s| s.is_finite() && **s >= min_score).count(),
            suggested_realms: Vec::new(),
            hint: match level {
                "high" => "Strong retrieval match; inspect the sources before answering.",
                "partial" => "Partial retrieval match. Try broader terms, a realm filter, or your configured digest.",
                _ => "No strong retrieval match. Try broader terms, a suggested realm, or your configured digest.",
            }.into(),
        }
    }

    pub fn first_line(&self) -> String {
        let best = self
            .best_score
            .map(|s| format!("{s:.2}"))
            .unwrap_or_else(|| "null".into());
        let realms = self
            .suggested_realms
            .iter()
            .map(|r| r.replace(['\n', '\r'], " "))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "coverage={} best={} results_above_min={} try realms: {} (retrieval heuristic)",
            self.level,
            best,
            self.results_above_min,
            if realms.is_empty() { "none" } else { &realms }
        )
    }
}

pub struct SearchOutcome {
    pub results: Vec<MemoryResult>,
    pub coverage: SearchCoverage,
}

impl Engine {
    pub async fn search_with_coverage(
        &self,
        query: &str,
        realm: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
        min_score: f32,
        visibility: Option<&str>,
    ) -> anyhow::Result<SearchOutcome> {
        self.config.search.coverage.validate()?;
        anyhow::ensure!(min_score.is_finite(), "min_score must be finite");
        let query_vector = self.make_embedder()?.embed(query).await?;
        // Use the same authoritative uncompressed probe for both retrieval paths.
        // No score threshold or pagination: misses and empty pages still get a signal.
        let scores = self
            .store
            .coverage_scores(&query_vector, COVERAGE_CANDIDATES, realm, tags, visibility)
            .await?;
        let mut coverage =
            SearchCoverage::from_scores(&scores, min_score, &self.config.search.coverage);
        coverage.suggested_realms = match self
            .suggest_coverage_realms(&query_vector, realm, tags, visibility)
            .await
        {
            Ok(realms) => realms,
            Err(error) => {
                tracing::warn!(%error, "Realm suggestions unavailable; retaining search coverage");
                Vec::new()
            }
        };
        let results = self
            .search_embedded(
                query,
                realm,
                tags,
                limit,
                offset,
                min_score,
                visibility,
                &query_vector,
            )
            .await?;
        Ok(SearchOutcome { results, coverage })
    }

    async fn suggest_coverage_realms(
        &self,
        query: &[f32],
        realm: Option<&str>,
        tags: Option<&[String]>,
        visibility: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let mut ranked: Vec<_> = self
            .store
            .list_realms()
            .await?
            .into_iter()
            .filter(|r| realm.is_none_or(|name| r.name == name))
            .filter_map(|r| valid_cosine(query, &r.centroid).map(|score| (r.name, score)))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut suggestions = Vec::new();
        for (name, _) in ranked {
            if suggestions.contains(&name) {
                continue;
            }
            // Check live eligible members; cached memory_count/centroids can be stale.
            if self
                .store
                .has_matching_memory(&name, tags, visibility)
                .await?
            {
                suggestions.push(name);
                if suggestions.len() == 3 {
                    break;
                }
            }
        }
        Ok(suggestions)
    }
}

fn valid_cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return None;
    }
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let (an, bn) = (norm(a), norm(b));
    if an == 0.0 || bn == 0.0 {
        return None;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
    Some((dot / (an * bn)).clamp(-1.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_thresholds_and_empty_candidates_are_explicit() {
        let config = CoverageConfig::default();
        for (scores, level, best, count) in [
            (vec![0.9, 0.6], "high", Some(0.9), 2),
            (vec![0.55], "high", Some(0.55), 1),
            (vec![0.4], "partial", Some(0.4), 0),
            (vec![0.31], "none", Some(0.31), 0),
            (vec![-0.2], "none", Some(-0.2), 0),
            (vec![], "none", None, 0),
        ] {
            let signal = SearchCoverage::from_scores(&scores, 0.5, &config);
            assert_eq!(
                (signal.level, signal.best_score, signal.results_above_min),
                (level, best, count)
            );
        }
    }
    #[test]
    fn invalid_centroids_are_not_ranked() {
        for vector in [vec![], vec![1.], vec![0., 0.], vec![f32::NAN, 1.]] {
            assert!(valid_cosine(&[1., 0.], &vector).is_none());
        }
        assert_eq!(valid_cosine(&[1., 0.], &[3., 0.]), Some(1.));
        assert_eq!(valid_cosine(&[1., 0.], &[0., 1.]), Some(0.));
    }
}
