//! Archived, idempotent application of the read-only duplicate plan.
use super::dedup::{self, DedupReport};
use super::Engine;
use crate::config::AppConfig;
use crate::storage::qdrant::{
    content_hash, ConsolidationOperation, MemoryPoint, MemoryPointWithVector, QdrantStore,
    RecoveryError,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(crate) fn invalidate_index_files(config: &AppConfig) -> anyhow::Result<()> {
    for path in [&config.turbovec.index_path, &config.turbovec.id_map_path] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Attribute accesses since the last merge to this point's own identity. Combining
/// maps with max, rather than adding aggregate counts, handles shared ancestry.
fn contributions(memory: &MemoryPoint) -> BTreeMap<String, u32> {
    let mut values = memory.dedup_access_contributions.clone();
    let accounted = values
        .values()
        .fold(0u32, |sum, count| sum.saturating_add(*count));
    let new_accesses = memory.access_count.saturating_sub(accounted);
    let own = values.entry(memory.id.clone()).or_default();
    *own = own.saturating_add(new_accesses);
    values
}

fn merged_payload(
    winner: &MemoryPoint,
    loser: &MemoryPoint,
    operation_id: &str,
) -> anyhow::Result<qdrant_client::Payload> {
    let mut accesses = contributions(winner);
    for (id, count) in contributions(loser) {
        let existing = accesses.entry(id).or_default();
        *existing = (*existing).max(count);
    }
    let access_count = accesses
        .values()
        .fold(0u32, |sum, count| sum.saturating_add(*count));
    let tags: BTreeSet<_> = winner.tags.iter().chain(&loser.tags).cloned().collect();
    let mut ancestry: BTreeSet<_> = winner
        .consolidated_from
        .iter()
        .chain(&loser.consolidated_from)
        .cloned()
        .collect();
    ancestry.insert(loser.id.clone());
    ancestry.remove(&winner.id);
    let mut applied: BTreeSet<_> = winner.dedup_applied_operations.iter().cloned().collect();
    applied.insert(operation_id.to_string());
    Ok(serde_json::json!({
        "tags": tags, "access_count": access_count,
        "dedup_access_contributions": accesses,
        "dedup_applied_operations": applied,
        "consolidated_from": ancestry,
        "consolidated_evidence": dedup::evidence_fingerprints(&[winner, loser]),
        "derived_by": if winner.derived_by.is_empty() { "dedup" } else { &winner.derived_by },
    })
    .try_into()?)
}

fn same_source(
    a: &crate::storage::qdrant::MemoryWithVector,
    b: &crate::storage::qdrant::MemoryWithVector,
) -> anyhow::Result<bool> {
    Ok(serde_json::to_value(a)? == serde_json::to_value(b)?)
}

async fn apply_operation(
    store: &QdrantStore,
    config: &AppConfig,
    op: &mut ConsolidationOperation,
) -> anyhow::Result<(bool, bool)> {
    super::redaction::ensure_clean(op)?;
    let before = op
        .dedup_winner
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Dedup journal lacks a winner"))?;
    anyhow::ensure!(
        op.sources.len() == 1 && op.sources[0].memory.id != before.id,
        "Invalid duplicate source identities"
    );
    let winner = store.get_memory_snapshot(&before.id).await?;
    let loser = store.get_memory_snapshot(&op.sources[0].memory.id).await?;
    let Some(winner) = winner else {
        store.update_consolidation_state(op, "cancelled").await?;
        return Ok((false, true));
    };
    let applied = winner
        .memory
        .dedup_applied_operations
        .contains(&op.summary_id);
    let Some(loser) = loser else {
        if applied {
            store
                .prune_duplicate_references(&op.sources[0].memory.id)
                .await?;
        }
        store
            .update_consolidation_state(op, if applied { "complete" } else { "cancelled" })
            .await?;
        return Ok((false, !applied));
    };
    // Replan against current original vectors, winner ordering, protection, scope,
    // and cooldowns. An interrupted archive must not authorize stale deletion.
    let pair = vec![
        MemoryPointWithVector {
            memory: winner.memory.clone(),
            vector: Some(winner.vector.clone()),
        },
        MemoryPointWithVector {
            memory: loser.memory.clone(),
            vector: Some(loser.vector.clone()),
        },
    ];
    let plan = dedup::plan_duplicates(&pair, &config.slumber)?;
    let valid = plan
        .candidates
        .iter()
        .any(|c| c.winner_id == winner.memory.id && c.loser_id == loser.memory.id);
    if !valid
        || !same_source(&loser, &op.sources[0])?
        || content_hash(&winner.memory.content) != op.summary_hash
        || dedup::source_fingerprint(&[&winner.memory]) != dedup::source_fingerprint(&[before])
    {
        store.update_consolidation_state(op, "cancelled").await?;
        return Ok((false, true));
    }
    if !applied {
        let payload = merged_payload(&winner.memory, &loser.memory, &op.summary_id)?;
        store
            .apply_dedup_payload(&winner.memory, payload, &op.summary_id)
            .await?;
    }
    // The archive and winner marker are durable before this guarded deletion.
    store.delete_consolidation_snapshot(&loser.memory).await?;
    store.prune_duplicate_references(&loser.memory.id).await?;
    store.update_consolidation_state(op, "complete").await?;
    tracing::info!(operation_id = %op.summary_id, winner_id = %winner.memory.id,
                   loser_id = %loser.memory.id, "Archived duplicate removed");
    Ok((true, false))
}

/// Caller owns the exclusive local maintenance gate. Dry run never reads/writes
/// archives or changes memory payloads, vectors, collections, or index files.
pub async fn remove_duplicates(
    store: &QdrantStore,
    config: &AppConfig,
) -> anyhow::Result<DedupReport> {
    let all = store.scroll_memories_internal(true).await?;
    let mut report = dedup::plan_duplicates(&all, &config.slumber)?;
    if config.slumber.dedup_dry_run {
        return Ok(report);
    }
    report.dry_run = false;
    invalidate_index_files(config)?;
    let mut handled = HashSet::new();
    for mut op in store.pending_dedup_operations().await? {
        for source in &op.sources {
            handled.insert(source.memory.id.clone());
        }
        report.operation_ids.push(op.summary_id.clone());
        let (removed, skipped) = apply_operation(store, config, &mut op).await?;
        report.deduplicated += usize::from(removed);
        report.skipped += usize::from(skipped);
    }
    for candidate in &report.candidates {
        if !handled.insert(candidate.loser_id.clone()) {
            continue;
        }
        let Some(winner) = store.get_memory_snapshot(&candidate.winner_id).await? else {
            report.skipped += 1;
            continue;
        };
        let Some(loser) = store.get_memory_snapshot(&candidate.loser_id).await? else {
            continue;
        };
        // Archive first, including the winner's pre-merge payload for audit.
        let mut op = ConsolidationOperation {
            version: 1,
            kind: "dedup".into(),
            dedup_winner: Some(winner.memory.clone()),
            summary_id: uuid::Uuid::new_v4().to_string(),
            summary_hash: content_hash(&winner.memory.content),
            created_at: chrono::Utc::now().to_rfc3339(),
            state: "prepared".into(),
            preserve_originals: false,
            embedding_provider: std::env::var("EMBEDDING_PROVIDER")
                .unwrap_or_else(|_| config.embedding.provider.clone()),
            embedding_model: if std::env::var("EMBEDDING_PROVIDER")
                .unwrap_or_else(|_| config.embedding.provider.clone())
                .starts_with("openai")
            {
                config.embedding.openai.model.clone()
            } else {
                std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| config.embedding.model.clone())
            },
            embedding_dimensions: loser.vector.len(),
            sources: vec![loser],
            cooldown_until: None,
        };
        store.archive_consolidation(&op).await?;
        tracing::info!(operation_id = %op.summary_id, winner_id = %winner.memory.id,
                       loser_id = %op.sources[0].memory.id, "Duplicate archived before metadata merge");
        report.operation_ids.push(op.summary_id.clone());
        let (removed, skipped) = apply_operation(store, config, &mut op).await?;
        report.deduplicated += usize::from(removed);
        report.skipped += usize::from(skipped);
    }
    Ok(report)
}

#[derive(Debug, serde::Serialize)]
pub struct DuplicateRestoreResult {
    pub operation_id: String,
    pub winner_id: String,
    pub source_ids: Vec<String>,
    pub restored: usize,
    pub retained: usize,
    pub already_restored: bool,
    pub no_dedup_until: Option<String>,
}

impl Engine {
    pub async fn restore_duplicate(
        &self,
        operation_id: &str,
    ) -> anyhow::Result<DuplicateRestoreResult> {
        let _guard = self.store.maintenance_guard().await;
        let mut op = self
            .store
            .get_consolidation(operation_id)
            .await?
            .ok_or_else(|| {
                RecoveryError::NotFound(format!("Duplicate journal not found: {}", operation_id))
            })?;
        if op.kind != "dedup" || op.sources.len() != 1 || op.dedup_winner.is_none() {
            return Err(RecoveryError::Conflict(
                "Journal is not a duplicate-removal operation".into(),
            )
            .into());
        }
        let winner_id = op.dedup_winner.as_ref().unwrap().id.clone();
        let ids: Vec<_> = op.sources.iter().map(|s| s.memory.id.clone()).collect();
        if ids[0] == winner_id {
            return Err(RecoveryError::Conflict("Invalid duplicate journal".into()).into());
        }
        if op.state == "restored" {
            return Ok(DuplicateRestoreResult {
                operation_id: operation_id.into(),
                winner_id,
                source_ids: ids,
                restored: 0,
                retained: 0,
                already_restored: true,
                no_dedup_until: op.cooldown_until,
            });
        }
        if !["prepared", "complete", "cancelled", "restoring"].contains(&op.state.as_str()) {
            return Err(RecoveryError::Conflict("Unknown duplicate journal state".into()).into());
        }
        let minimum = chrono::Utc::now() + chrono::Duration::days(30);
        let until = match op.cooldown_until.as_deref() {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| RecoveryError::Conflict("Invalid duplicate journal cooldown".into()))?
                .with_timezone(&chrono::Utc)
                .max(minimum),
            None => minimum,
        };
        op.cooldown_until = Some(until.to_rfc3339());
        self.store
            .update_consolidation_state(&mut op, "restoring")
            .await?;
        self.invalidate_search_index().await?;
        let mut restored = 0;
        let mut retained = 0;
        for source in &op.sources {
            if let Some(existing) = self.store.get_memory(&source.memory.id).await? {
                let later = |value: Option<&str>| {
                    value
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&chrono::Utc).max(until))
                        .unwrap_or(until)
                        .to_rfc3339()
                };
                self.store
                    .update_memory_payload(
                        &existing.id,
                        serde_json::json!({
                            "no_dedup_until": later(existing.no_dedup_until.as_deref()),
                            "no_consolidate_until": later(existing.no_consolidate_until.as_deref()),
                        })
                        .try_into()?,
                    )
                    .await?;
                retained += 1;
            } else {
                super::redaction::ensure_memory(&source.memory)?;
                let vector = self.make_embedder()?.embed(&source.memory.content).await?;
                anyhow::ensure!(
                    vector.len() == self.embed_dimensions as usize
                        && vector.iter().all(|v| v.is_finite()),
                    "Invalid restored duplicate embedding"
                );
                let mut memory = source.memory.clone();
                memory.no_dedup_until = Some(until.to_rfc3339());
                memory.no_consolidate_until = Some(until.to_rfc3339());
                self.store.replace_memory(&memory, &vector).await?;
                restored += 1;
            }
        }
        for source_id in &ids {
            let source = self
                .store
                .get_memory_snapshot(source_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Restored duplicate missing"))?;
            anyhow::ensure!(
                [
                    source.memory.no_dedup_until,
                    source.memory.no_consolidate_until
                ]
                .iter()
                .all(|s| s
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .is_some_and(|time| time >= until)),
                "Duplicate cooldown failed read-back"
            );
        }
        // Winner metadata remains: subtracting old counts/tags would discard newer
        // activity. Contribution maps prevent future merges from adding it twice.
        self.store
            .update_consolidation_state(&mut op, "restored")
            .await?;
        Ok(DuplicateRestoreResult {
            operation_id: operation_id.into(),
            winner_id,
            source_ids: ids,
            restored,
            retained,
            already_restored: false,
            no_dedup_until: op.cooldown_until,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn memory(id: &str, count: u32) -> MemoryPoint {
        serde_json::from_value(serde_json::json!({
            "id": id, "content": id, "summary": null, "source_file": null,
            "realm_id": null, "realm_name": "r", "importance": 1., "upvotes": 0,
            "tags": [], "ingested_at": "2026-11-01T00:00:00Z", "last_accessed": "",
            "access_count": count, "chunk_type": "manual", "heading": null, "source_hash": ""
        }))
        .unwrap()
    }
    fn merge(winner: &MemoryPoint, loser: &MemoryPoint) -> MemoryPoint {
        let payload: serde_json::Value = merged_payload(winner, loser, "op").unwrap().into();
        let mut value = serde_json::to_value(winner).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn merging_and_retrying_accesses_is_idempotent_and_preserves_ancestry() {
        let mut a = memory("a", 3);
        a.tags = vec!["first".into()];
        a.consolidated_from = vec!["older".into()];
        let mut b = memory("b", 4);
        b.tags = vec!["second".into()];
        let merged = merge(&a, &b);
        assert_eq!(merged.access_count, 7);
        assert_eq!(merge(&merged, &b).access_count, 7);
        assert_eq!(merged.consolidated_from, ["b", "older"]);
        assert_eq!(merged.tags, ["first", "second"]);
        let mut active = merged.clone();
        active.access_count += 2;
        let mut restored_b = b.clone();
        restored_b.access_count += 1;
        assert_eq!(merge(&active, &restored_b).access_count, 10);
    }
    #[test]
    fn shared_ancestry_is_not_counted_twice_and_counts_saturate() {
        let a = memory("a", 3);
        let b = memory("b", 4);
        let c = memory("c", 2);
        let ab = merge(&a, &b);
        let cb = merge(&c, &b);
        assert_eq!(merge(&ab, &cb).access_count, 9);
        assert_eq!(merge(&memory("large", u32::MAX), &b).access_count, u32::MAX);
    }
}
