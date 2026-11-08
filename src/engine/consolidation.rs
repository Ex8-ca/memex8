//! Recoverable consolidation. Qdrant is the durable source of truth; all local
//! maintenance/content replacement shares one exclusive gate with recovery.
use super::Engine;
use crate::storage::qdrant::{
    content_hash, ConsolidationOperation, RecoveryError, UnconsolidateResult,
};

impl Engine {
    pub async fn unconsolidate(&self, id: &str) -> anyhow::Result<UnconsolidateResult> {
        let _guard = self.store.maintenance_guard().await;
        let summary = self.store.get_memory(id).await?;
        let mut operation = match self.store.get_consolidation(id).await? {
            Some(operation) => operation,
            None => {
                let memory = summary.as_ref().ok_or_else(|| {
                    RecoveryError::NotFound(format!(
                        "Memory or consolidation journal not found: {}",
                        id
                    ))
                })?;
                if memory.consolidated_from.is_empty() {
                    return Err(RecoveryError::Conflict(
                        "Memory has no recoverable consolidation sources".into(),
                    )
                    .into());
                }
                let mut sources = Vec::new();
                for source_id in &memory.consolidated_from {
                    let source = self.store.get_memory_snapshot(source_id).await?.ok_or_else(||
                        RecoveryError::Conflict(format!("Source {} is missing and was never archived; legacy consolidation cannot be undone", source_id))
                    )?;
                    sources.push(source);
                }
                // Legacy/preserved originals: checkpoint current points without ever
                // overwriting newer edits with an older approximation.
                let operation = ConsolidationOperation {
                    version: 1,
                    summary_id: id.into(),
                    summary_hash: content_hash(&memory.content),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    state: "complete".into(),
                    preserve_originals: true,
                    embedding_provider: "unknown".into(),
                    embedding_model: "unknown".into(),
                    embedding_dimensions: sources.first().map_or(0, |s| s.vector.len()),
                    sources,
                    cooldown_until: None,
                };
                self.store.archive_consolidation(&operation).await?;
                operation
            }
        };
        let ids: Vec<_> = operation
            .sources
            .iter()
            .map(|s| s.memory.id.clone())
            .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        if ids.is_empty() || unique.len() != ids.len() || ids.iter().any(|source| source == id) {
            return Err(
                RecoveryError::Conflict("Invalid consolidation source identities".into()).into(),
            );
        }
        if operation.state == "restored" {
            return Ok(UnconsolidateResult {
                summary_id: id.into(),
                source_ids: ids,
                restored: 0,
                retained: 0,
                already_restored: true,
                no_consolidate_until: operation.cooldown_until,
            });
        }
        if !["prepared", "complete", "restoring"].contains(&operation.state.as_str()) {
            return Err(
                RecoveryError::Conflict("Unknown consolidation journal state".into()).into(),
            );
        }
        if let Some(summary) = &summary {
            if summary.is_certified()
                || content_hash(&summary.content) != operation.summary_hash
                || summary.consolidated_from != ids
            {
                return Err(RecoveryError::Conflict(
                    "Summary was edited or certified; revoke certification or resolve the edit before undo".into(),
                ).into());
            }
        }
        // A retry weeks after an interruption still gives newly restored points
        // a full cooldown. Completed retries return above without extending it.
        let minimum_cooldown = chrono::Utc::now() + chrono::Duration::days(30);
        let cooldown_time = match operation.cooldown_until.as_deref() {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| {
                    RecoveryError::Conflict(
                        "Consolidation journal contains an invalid cooldown".into(),
                    )
                })?
                .with_timezone(&chrono::Utc)
                .max(minimum_cooldown),
            None => minimum_cooldown,
        };
        let cooldown = cooldown_time.to_rfc3339();
        operation.cooldown_until = Some(cooldown.clone());
        self.store
            .update_consolidation_state(&mut operation, "restoring")
            .await?;

        // Invalidate both memory and disk caches before restoring sources. Search
        // falls back to Qdrant until slumber builds a fresh compressed index.
        *self.turbovec_index.write().await = None;
        for path in [
            &self.config.turbovec.index_path,
            &self.config.turbovec.id_map_path,
        ] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut restored = 0;
        let mut retained = 0;
        let mut embedder = None;
        for snapshot in &operation.sources {
            if let Some(existing) = self.store.get_memory(&snapshot.memory.id).await? {
                // Keep all current content and metadata, even when newer than the archive.
                let until = existing
                    .no_consolidate_until
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .filter(|v| v.with_timezone(&chrono::Utc) > cooldown_time)
                    .map(|v| v.to_rfc3339())
                    .unwrap_or_else(|| cooldown.clone());
                self.store
                    .update_memory_payload(
                        &existing.id,
                        serde_json::json!({"no_consolidate_until": until}).try_into()?,
                    )
                    .await?;
                retained += 1;
            } else {
                if embedder.is_none() {
                    embedder = Some(self.make_embedder()?);
                }
                // Re-embed missing points with the current model. Archived vectors
                // remain available for audit and future compatible-model restores.
                let vector = embedder
                    .as_ref()
                    .unwrap()
                    .embed(&snapshot.memory.content)
                    .await?;
                anyhow::ensure!(
                    vector.len() == self.embed_dimensions as usize
                        && vector.iter().all(|v| v.is_finite()),
                    "Restored embedding has unexpected dimensions or non-finite values"
                );
                let mut memory = snapshot.memory.clone();
                memory.no_consolidate_until = Some(cooldown.clone());
                self.store.replace_memory(&memory, &vector).await?;
                restored += 1;
            }
        }
        // A failed restore never removes the summary. Each source must be readable
        // before the final deletion; partial work stays retryable in `restoring`.
        for source in &ids {
            let restored = self
                .store
                .get_memory_snapshot(source)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Source {} is not durable after restore", source))?;
            anyhow::ensure!(
                restored
                    .memory
                    .no_consolidate_until
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .is_some_and(|v| v >= cooldown_time),
                "Source {} cooldown failed read-back",
                source
            );
        }
        if let Some(summary) = summary {
            self.store.delete_consolidation_snapshot(&summary).await?;
        }
        self.store
            .update_consolidation_state(&mut operation, "restored")
            .await?;
        tracing::info!(summary_id = id, restored, retained, "Consolidation undone");
        Ok(UnconsolidateResult {
            summary_id: id.into(),
            source_ids: ids,
            restored,
            retained,
            already_restored: false,
            no_consolidate_until: Some(cooldown),
        })
    }
}
