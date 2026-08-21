//! Query intent classification for score weight adjustment.
//!
//! Regex-based classifier that infers the user's search intent from the
//! query string, then returns weight biases to apply to the three scoring
//! components memex8 actually uses (vector, importance, recency).
//!
//! Mnemosyne's original adjusts (vector, FTS, importance) — we collapse
//! FTS out since memex8 has no FTS layer, and add recency in its place.

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    Temporal,
    Factual,
    Entity,
    Preference,
    Procedural,
    General,
}

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub vector_bias: f32,
    pub importance_bias: f32,
    pub recency_bias: f32,
}

impl Weights {
    pub const DEFAULT: Self = Self {
        vector_bias: 1.0,
        importance_bias: 1.0,
        recency_bias: 1.0,
    };
}

/// Classify a query into an intent category.
///
/// Confidence thresholds are hand-tuned (mirror Mnemosyne's regex
/// confidence). Returns Intent::General with all biases 1.0 if nothing
/// matches.
pub fn classify(query: &str) -> (Intent, f32) {
    let lower = query.to_lowercase();

    // Order matters: most-specific patterns first. Each tuple is
    // (intent, patterns, confidence, biases).
    let rules: &[(&str, &[&str], f32, Weights)] = &[
        (
            "preference",
            &[
                r"\b(like|likes|liked|prefer|prefers|preferred|favorite|favourite|love|loves|hate|hates|enjoy|enjoys)\b",
                r"\bwhat\s+does\s+\w+\s+(like|prefer|love|hate|enjoy)\b",
                r"\b\w+'s\s+(favorite|favourite)\b",
            ],
            0.85,
            Weights { vector_bias: 1.0, importance_bias: 1.4, recency_bias: 0.5 },
        ),
        (
            "temporal",
            &[
                r"\b(when|last|yesterday|today|tomorrow|ago|before|after|since|until|during|recently|lately)\b",
                r"\b(monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b",
                r"\b(january|february|march|april|may|june|july|august|september|october|november|december)\b",
                r"\b\d{4}-\d{2}-\d{2}\b",
                r"\b\d{1,2}[/-]\d{1,2}[/-]\d{2,4}\b",
                r"\b(this|next|last)\s+(week|month|year|monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b",
                r"\b\d+\s+(day|week|month|year|hour|minute)s?\s+(ago|from now|later|earlier)\b",
            ],
            0.9,
            Weights { vector_bias: 0.7, importance_bias: 0.8, recency_bias: 1.5 },
        ),
        (
            "factual",
            &[
                r"\bwhat\s+is\b",
                r"\bwho\s+is\b",
                r"\bwhere\s+is\b",
                r"\b(definition|define|explain|meaning)\b",
                r"\bhow\s+(many|much|long|far)\b",
            ],
            0.8,
            Weights { vector_bias: 1.0, importance_bias: 1.2, recency_bias: 0.7 },
        ),
        (
            "entity",
            &[
                r"\b(who|what)\s+(is|was|are|were)\s+\w+",
                r"\babout\s+\w+\b",
            ],
            0.7,
            Weights { vector_bias: 1.2, importance_bias: 1.1, recency_bias: 0.8 },
        ),
        (
            "procedural",
            &[
                r"\bhow\s+(do|does|did|can|could|would|should|to)\b",
                r"\b(setup|install|configure|deploy|build|run|start|stop|restart|fix|debug|troubleshoot)\b",
                r"\b(step|steps|guide|tutorial|instructions?)\b",
            ],
            0.8,
            Weights { vector_bias: 1.3, importance_bias: 1.0, recency_bias: 0.6 },
        ),
    ];

    for (name, patterns, conf, _weights) in rules {
        for pat in *patterns {
            if let Ok(re) = Regex::new(pat) {
                if re.is_match(&lower) {
                    let intent = match *name {
                        "temporal" => Intent::Temporal,
                        "factual" => Intent::Factual,
                        "entity" => Intent::Entity,
                        "preference" => Intent::Preference,
                        "procedural" => Intent::Procedural,
                        _ => Intent::General,
                    };
                    return (intent, *conf);
                }
            }
        }
    }

    (Intent::General, 0.5)
}

/// Get weight biases for an intent. Convenience wrapper around classify().
pub fn weights_for(query: &str) -> Weights {
    let (intent, _conf) = classify(query);
    match intent {
        Intent::Temporal => Weights { vector_bias: 0.7, importance_bias: 0.8, recency_bias: 1.5 },
        Intent::Factual => Weights { vector_bias: 1.0, importance_bias: 1.2, recency_bias: 0.7 },
        Intent::Entity => Weights { vector_bias: 1.2, importance_bias: 1.1, recency_bias: 0.8 },
        Intent::Preference => Weights { vector_bias: 1.0, importance_bias: 1.4, recency_bias: 0.5 },
        Intent::Procedural => Weights { vector_bias: 1.3, importance_bias: 1.0, recency_bias: 0.6 },
        Intent::General => Weights::DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_queries() {
        let cases = [
            "when did we deploy the last release",
            "what happened last week",
            "what did marc do yesterday",
            "meetings in 2026-05-12",
            "5 days ago",
        ];
        for q in cases {
            let (intent, conf) = classify(q);
            assert_eq!(intent, Intent::Temporal, "expected Temporal for: {}", q);
            assert!(conf > 0.5);
        }
    }

    #[test]
    fn test_factual_queries() {
        let cases = [
            "what is the database password",
            "who is the maintainer",
            "where is the config file",
        ];
        for q in cases {
            let (intent, _) = classify(q);
            assert_eq!(intent, Intent::Factual, "expected Factual for: {}", q);
        }
    }

    #[test]
    fn test_preference_queries() {
        let cases = [
            "what does deanna like to cook",
            "marc prefers vim",
            "what is marc's favorite color",
        ];
        for q in cases {
            let (intent, _) = classify(q);
            assert_eq!(intent, Intent::Preference, "expected Preference for: {}", q);
        }
    }

    #[test]
    fn test_procedural_queries() {
        let cases = [
            "how do I deploy the container",
            "how to install memex8",
            "debug the build failure",
        ];
        for q in cases {
            let (intent, _) = classify(q);
            assert_eq!(intent, Intent::Procedural, "expected Procedural for: {}", q);
        }
    }

    #[test]
    fn test_general_fallback() {
        let cases = ["memex8", "docker compose", "agent memory"];
        for q in cases {
            let (intent, conf) = classify(q);
            assert_eq!(intent, Intent::General, "expected General for: {}", q);
            assert!((conf - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn test_weights_for_preference_boosts_importance() {
        let w = weights_for("what does marc prefer");
        assert!(w.importance_bias > 1.0);
        assert!(w.recency_bias < 1.0);
    }

    #[test]
    fn test_weights_for_temporal_boosts_recency() {
        let w = weights_for("what happened last week");
        assert!(w.recency_bias > 1.0);
    }
}
