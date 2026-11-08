//! Read-only duplicate planning. No storage mutation belongs in this module.
use crate::config::SlumberConfig;
use crate::storage::qdrant::{normalize_visibility, MemoryPoint, MemoryPointWithVector};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DuplicateCandidate {
    pub winner_id: String,
    pub loser_id: String,
    pub realm_name: String,
    pub reason: String,
    pub cosine: Option<f32>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DedupReport {
    pub memories_scanned: usize,
    pub candidates: Vec<DuplicateCandidate>,
    /// Actual writes are deliberately unavailable in the detection release.
    pub deduplicated: usize,
    pub dry_run: bool,
    pub protected_duplicates: usize,
    pub invalid_vectors: usize,
    pub comparisons: usize,
    pub truncated_realms: Vec<String>,
}

fn memory_type(memory: &MemoryPoint) -> &str {
    if memory.memory_type.is_empty() {
        "general"
    } else {
        &memory.memory_type
    }
}

fn compatible(a: &MemoryPoint, b: &MemoryPoint) -> bool {
    let long_lived = |t| matches!(t, "profile" | "preference" | "entity" | "relationship");
    let (at, bt) = (memory_type(a), memory_type(b));
    normalize_visibility(&a.visibility) == normalize_visibility(&b.visibility)
        && (at == bt || (!long_lived(at) && !long_lived(bt)))
}

fn certified(memory: &MemoryPoint) -> bool {
    memory.is_certified()
}

fn valid_vector(vector: &[f32]) -> bool {
    !vector.is_empty() && vector.iter().all(|v| v.is_finite()) && vector.iter().any(|v| *v != 0.0)
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || !valid_vector(a) || !valid_vector(b) {
        return None;
    }
    let (mut dot, mut an, mut bn) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in a.iter().zip(b) {
        let (a, b) = (a as f64, b as f64);
        dot += a * b;
        an += a * a;
        bn += b * b;
    }
    Some((dot / (an.sqrt() * bn.sqrt())).clamp(-1.0, 1.0) as f32)
}

fn timestamp(memory: &MemoryPoint) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(&memory.ingested_at).ok()
}

/// Each loser is compared directly with its retained winner. Connected-component
/// clustering would incorrectly delete the endpoint of an A~B~C similarity chain.
pub fn plan_duplicates(
    memories: &[MemoryPointWithVector],
    config: &SlumberConfig,
) -> anyhow::Result<DedupReport> {
    config.validate_dedup()?;
    let mut report = DedupReport {
        memories_scanned: memories.len(),
        dry_run: true,
        invalid_vectors: memories
            .iter()
            .filter(|m| m.vector.as_deref().is_none_or(|v| !valid_vector(v)))
            .count(),
        ..Default::default()
    };
    let mut realms: BTreeMap<(bool, &str), Vec<&MemoryPointWithVector>> = BTreeMap::new();
    for memory in memories {
        // IDs are authoritative; names are a fallback for older summaries.
        let realm = match memory
            .memory
            .realm_id
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            Some(id) => (true, id),
            None => (false, memory.memory.realm_name.as_str()),
        };
        realms.entry(realm).or_default().push(memory);
    }
    for (_, mut members) in realms {
        members.sort_by(|a, b| {
            let (a, b) = (&a.memory, &b.memory);
            certified(b)
                .cmp(&certified(a))
                .then_with(|| b.upvotes.cmp(&a.upvotes))
                .then_with(|| {
                    let finite = |v: f32| if v.is_finite() { v } else { f32::NEG_INFINITY };
                    finite(b.importance).total_cmp(&finite(a.importance))
                })
                .then_with(|| timestamp(b).cmp(&timestamp(a)))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut losers = HashSet::new();
        let mut protected = HashSet::new();
        let mut comparisons = 0;
        'realm: for (i, winner) in members.iter().enumerate() {
            if losers.contains(&winner.memory.id) {
                continue;
            }
            for loser in &members[i + 1..] {
                if losers.contains(&loser.memory.id) {
                    continue;
                }
                if comparisons == config.dedup_max_comparisons_per_realm {
                    report
                        .truncated_realms
                        .push(winner.memory.realm_name.clone());
                    break 'realm;
                }
                comparisons += 1;
                if !compatible(&winner.memory, &loser.memory) {
                    continue;
                }
                let exact = !winner.memory.source_hash.is_empty()
                    && winner.memory.source_hash == loser.memory.source_hash
                    && !winner.memory.content.trim().is_empty()
                    && winner.memory.content == loser.memory.content;
                let score = winner
                    .vector
                    .as_deref()
                    .zip(loser.vector.as_deref())
                    .and_then(|(a, b)| cosine(a, b));
                if !exact && score.is_none_or(|s| s < config.dedup_threshold) {
                    continue;
                }
                if certified(&loser.memory) {
                    protected.insert(loser.memory.id.clone());
                    continue;
                }
                losers.insert(loser.memory.id.clone());
                report.candidates.push(DuplicateCandidate {
                    winner_id: winner.memory.id.clone(),
                    loser_id: loser.memory.id.clone(),
                    realm_name: winner.memory.realm_name.clone(),
                    reason: if exact {
                        "exact_content_and_hash"
                    } else {
                        "near_duplicate"
                    }
                    .into(),
                    cosine: score,
                });
            }
        }
        report.comparisons += comparisons;
        report.protected_duplicates += protected.len();
    }
    Ok(report)
}

/// Derived summaries are never fed back into consolidation. With originals
/// preserved, skip a source set already represented by a recorded summary.
pub fn consolidation_inputs<'a>(members: &[&'a MemoryPoint]) -> Vec<&'a MemoryPoint> {
    members
        .iter()
        .copied()
        .filter(|m| {
            m.chunk_type != "consolidated"
                && m.consolidated_from.is_empty()
                && !certified(m)
                && !consolidation_on_cooldown(m)
        })
        .collect()
}

/// Malformed cooldowns fail closed until an operator repairs the timestamp.
pub fn consolidation_on_cooldown(memory: &MemoryPoint) -> bool {
    memory.no_consolidate_until.as_deref().is_some_and(|until| {
        chrono::DateTime::parse_from_rfc3339(until).map_or(true, |time| time > chrono::Utc::now())
    })
}

pub fn evidence_fingerprints(sources: &[&MemoryPoint]) -> Vec<String> {
    let mut evidence: Vec<_> = sources
        .iter()
        .flat_map(|source| {
            if source.consolidated_evidence.is_empty() {
                vec![source_fingerprint(&[source])]
            } else {
                source.consolidated_evidence.clone()
            }
        })
        .collect();
    evidence.sort();
    evidence.dedup();
    evidence
}

pub fn already_consolidated(sources: &[&MemoryPoint], existing: &[&MemoryPoint]) -> bool {
    let fingerprint = source_fingerprint(sources);
    let covered: std::collections::HashSet<_> = existing
        .iter()
        .flat_map(|m| m.consolidated_evidence.iter())
        .collect();
    sources.len() < 2
        || evidence_fingerprints(sources)
            .iter()
            .all(|item| covered.contains(item))
        || existing.iter().any(|summary| {
            !summary.consolidated_from.is_empty()
                && summary.source_hash == fingerprint
                && sources
                    .iter()
                    .all(|m| summary.consolidated_from.contains(&m.id))
        })
}

/// Include content and metadata so editing a source under the same ID counts as
/// new evidence. Stable sorting makes the fingerprint independent of ranking.
pub fn source_fingerprint(sources: &[&MemoryPoint]) -> String {
    use sha2::{Digest, Sha256};
    let mut values: Vec<_> = sources
        .iter()
        .map(|m| (&m.id, &m.content, &m.heading, &m.memory_type))
        .collect();
    values.sort();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&values).expect("string tuples serialize"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn point(id: &str, vector: &[f32]) -> MemoryPointWithVector {
        let memory = serde_json::from_value(serde_json::json!({
            "id": id, "content": format!("memory {id}"), "summary": null,
            "source_file": null, "realm_id": "r1", "realm_name": "Realm",
            "importance": 1.0, "upvotes": 0, "tags": [],
            "ingested_at": "2026-11-01T00:00:00Z", "last_accessed": "",
            "access_count": 0, "chunk_type": "manual", "heading": null,
            "source_hash": "file-hash", "memory_type": "event", "visibility": "private"
        }))
        .unwrap();
        MemoryPointWithVector {
            memory,
            vector: Some(vector.to_vec()),
        }
    }

    fn plan(points: &[MemoryPointWithVector]) -> DedupReport {
        plan_duplicates(points, &AppConfig::default().slumber).unwrap()
    }

    #[test]
    fn three_near_duplicates_and_one_distinct_are_only_proposed() {
        let points = vec![
            point("a", &[1.0, 0.0]),
            point("b", &[1.0, 0.1]),
            point("c", &[1.0, -0.1]),
            point("d", &[0.0, 1.0]),
        ];
        let before =
            serde_json::to_value(&points.iter().map(|p| &p.memory).collect::<Vec<_>>()).unwrap();
        let report = plan(&points);
        assert_eq!(report.candidates.len(), 2);
        assert!(report
            .candidates
            .iter()
            .all(|p| p.winner_id == "a" && p.cosine.unwrap() >= 0.97));
        assert!(report.dry_run);
        assert_eq!(report.deduplicated, 0);
        assert_eq!(
            before,
            serde_json::to_value(&points.iter().map(|p| &p.memory).collect::<Vec<_>>()).unwrap()
        );
    }

    #[test]
    fn similarity_chains_do_not_delete_distinct_endpoints() {
        let points = [
            point("a", &[1.0, 0.0]),
            point("b", &[0.97, 0.2431]),
            point("c", &[0.88, 0.475]),
        ];
        assert!(
            cosine(
                points[1].vector.as_ref().unwrap(),
                points[2].vector.as_ref().unwrap()
            )
            .unwrap()
                > 0.95
        );
        let report = plan(&points);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].loser_id, "b");
    }

    #[test]
    fn shared_file_hash_is_not_content_identity() {
        let mut points = [point("a", &[1.0, 0.0]), point("b", &[0.0, 1.0])];
        assert!(plan(&points).candidates.is_empty());
        points[1].memory.content = points[0].memory.content.clone();
        points[1].vector = None;
        let report = plan(&points);
        assert_eq!(report.candidates[0].reason, "exact_content_and_hash");
        assert_eq!(report.invalid_vectors, 1);
    }

    #[test]
    fn protected_types_visibility_and_realms_are_separate() {
        let base = point("a", &[1.0, 0.0]);
        for field in ["type", "visibility", "realm"] {
            let mut other = point("b", &[1.0, 0.0]);
            match field {
                "type" => other.memory.memory_type = "profile".into(),
                "visibility" => other.memory.visibility = "public".into(),
                _ => other.memory.realm_id = Some("r2".into()),
            }
            assert!(plan(&[base.clone(), other]).candidates.is_empty());
        }
    }

    #[test]
    fn certified_duplicates_survive_and_outrank_other_winners() {
        let mut points = [
            point("a", &[1.0, 0.0]),
            point("b", &[1.0, 0.0]),
            point("c", &[1.0, 0.0]),
        ];
        points[0].memory.upvotes = 100;
        points[1].memory.verification_status = "certified".into();
        points[2].memory.verification_status = "certified".into();
        let report = plan(&points);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].winner_id, "b");
        assert_eq!(report.candidates[0].loser_id, "a");
        assert_eq!(report.protected_duplicates, 1);
    }

    #[test]
    fn votes_then_importance_then_actual_time_then_id_determine_winner() {
        let mut points = [point("a", &[1.0, 0.0]), point("b", &[1.0, 0.0])];
        points[1].memory.upvotes = 1;
        assert_eq!(plan(&points).candidates[0].winner_id, "b");
        points[0].memory.upvotes = 1;
        points[1].memory.importance = 2.0;
        assert_eq!(plan(&points).candidates[0].winner_id, "b");
        points[0].memory.importance = 2.0;
        points[1].memory.ingested_at = "2026-10-31T23:30:00-02:00".into();
        assert_eq!(plan(&points).candidates[0].winner_id, "b");
        points[1].memory.ingested_at = points[0].memory.ingested_at.clone();
        points.reverse();
        assert_eq!(plan(&points).candidates[0].winner_id, "a");
    }

    #[test]
    fn invalid_vectors_never_create_near_candidates() {
        for vector in [
            vec![],
            vec![0.0, 0.0],
            vec![f32::NAN, 0.0],
            vec![f32::INFINITY, 0.0],
            vec![1.0],
        ] {
            assert!(plan(&[point("a", &[1.0, 0.0]), point("b", &vector)])
                .candidates
                .is_empty());
        }
    }

    #[test]
    fn work_limit_is_visible_and_never_infers_unexamined_losers() {
        let points = [
            point("a", &[1.0, 0.0]),
            point("b", &[0.0, 1.0]),
            point("c", &[-1.0, 0.0]),
        ];
        let mut config = AppConfig::default().slumber;
        config.dedup_max_comparisons_per_realm = 1;
        let report = plan_duplicates(&points, &config).unwrap();
        assert_eq!(report.comparisons, 1);
        assert_eq!(report.truncated_realms, ["Realm"]);
        assert!(report.candidates.is_empty());
    }

    #[test]
    fn provenance_blocks_repeated_source_sets_but_allows_new_evidence() {
        let sources = [point("a", &[1.0]), point("b", &[1.0])];
        let mut summary = point("summary", &[1.0]);
        summary.memory.consolidated_from = vec!["a".into(), "b".into()];
        let refs = [&sources[0].memory, &sources[1].memory];
        summary.memory.source_hash = source_fingerprint(&refs);
        assert!(already_consolidated(&refs, &[&summary.memory]));
        summary.memory.consolidated_from = vec!["a".into(), "older".into()];
        assert!(!already_consolidated(&refs, &[&summary.memory]));
    }

    #[test]
    fn edits_to_existing_source_ids_count_as_new_evidence() {
        let mut sources = [point("a", &[1.0]), point("b", &[1.0])];
        let mut summary = point("summary", &[1.0]);
        summary.memory.consolidated_from = vec!["a".into(), "b".into()];
        summary.memory.source_hash = source_fingerprint(&[&sources[0].memory, &sources[1].memory]);
        sources[0].memory.content = "Corrected fact".into();
        assert!(!already_consolidated(
            &[&sources[0].memory, &sources[1].memory],
            &[&summary.memory]
        ));
    }

    #[test]
    fn summaries_and_certified_points_are_excluded_from_consolidation_inputs() {
        let mut summary = point("summary", &[1.0]);
        summary.memory.chunk_type = "consolidated".into();
        let mut certified = point("certified", &[1.0]);
        certified.memory.verification_status = "certified".into();
        let source = point("source", &[1.0]);
        let inputs = consolidation_inputs(&[&summary.memory, &certified.memory, &source.memory]);
        assert_eq!(
            inputs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["source"]
        );
        assert!(consolidation_inputs(&[&summary.memory]).is_empty());
    }
    #[test]
    fn evidence_coverage_handles_overlapping_and_ancestral_summaries() {
        let a = point("a", &[1.0]);
        let b = point("b", &[1.0]);
        let c = point("c", &[1.0]);
        let mut first = point("first", &[1.0]);
        first.memory.consolidated_evidence = evidence_fingerprints(&[&a.memory, &b.memory]);
        let mut second = point("second", &[1.0]);
        second.memory.consolidated_evidence = evidence_fingerprints(&[&first.memory, &c.memory]);
        assert!(already_consolidated(
            &[&a.memory, &b.memory],
            &[&second.memory]
        ));
        assert!(!already_consolidated(
            &[&b.memory, &c.memory],
            &[&first.memory]
        ));
        assert!(already_consolidated(
            &[&a.memory, &c.memory],
            &[&first.memory, &second.memory]
        ));
    }

    #[test]
    fn cooldown_blocks_consolidation_and_expires_by_timestamp() {
        let mut memory = point("source", &[1.0]).memory;
        memory.no_consolidate_until =
            Some((chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339());
        assert!(consolidation_inputs(&[&memory]).is_empty());
        memory.no_consolidate_until =
            Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
        assert_eq!(consolidation_inputs(&[&memory]).len(), 1);
        memory.no_consolidate_until = Some("invalid".into());
        assert!(consolidation_inputs(&[&memory]).is_empty());
    }
}
