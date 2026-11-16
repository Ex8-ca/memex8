//! Explicit REST evidence slice; intentionally refuses unsupported lifecycle operations.
use super::Engine;
use crate::api::error::ApiError;
use crate::storage::evidence::{CaptureRequest, EvidenceRef, EvidenceStore, SourceRecord};

impl Engine {
    pub(super) fn erased_ids(&self) -> anyhow::Result<std::collections::HashSet<String>> {
        Ok(self
            .evidence_store()?
            .map_or(Ok(Default::default()), |s| s.erased_ids())?)
    }
    pub(super) fn evidence_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.config.turbovec.index_path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("data"))
            .join("evidence.sqlite3")
    }
    pub(super) fn evidence_store(&self) -> anyhow::Result<Option<EvidenceStore>> {
        let path = self.evidence_path();
        if path.exists() {
            Ok(Some(EvidenceStore::open(&path)?))
        } else {
            Ok(None)
        }
    }
    /// Called regardless of feature flag, so disabling capture cannot bypass retention gates.
    pub(super) fn refuse_evidence_transfer(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.config.evidence.enabled, "evidence enabled: logical transfer and slumber unsupported; use a stopped-writer paired volume backup");
        if let Some(store) = self.evidence_store()? {
            anyhow::ensure!(!store.has_records()?, "evidence present: logical backup/export/import/restore and slumber are unsupported; use a stopped-writer paired volume backup");
        }
        Ok(())
    }
    /// Complete every durable erasure work item before startup can serve reads or capture.
    pub(super) async fn reconcile_pending_erasures(&self) -> anyhow::Result<()> {
        let _guard = self.store.maintenance_guard().await;
        if let Some(ledger) = self.evidence_store()? {
            let pending = ledger.pending_erasures()?;
            if !pending.is_empty() {
                for intent in pending {
                    self.store
                        .delete_projection_after_erasure_intent(&intent.memory_id)
                        .await?;
                    ledger.finish_erase(&intent.memory_id)?;
                }
                // A loaded TurboVec index can still contain erased IDs; discard it
                // and the files behind it, exactly like a normal deletion.
                self.invalidate_compressed_index().await;
            }
        }
        Ok(())
    }

    pub async fn refuse_evidence_mutation(&self, id: &str) -> anyhow::Result<()> {
        if let Some(store) = self.evidence_store()? {
            anyhow::ensure!(!store.contains_memory(id)?, "evidence-linked mutation/deletion unsupported: source erasure and revisions are not implemented");
        }
        if let Some(memory) = self.store.get_memory(id).await? {
            anyhow::ensure!(
                memory.evidence_refs.is_empty(),
                "evidence-linked mutation/deletion unsupported"
            );
        }
        Ok(())
    }
    pub(super) async fn refuse_evidence_projection_transfer(&self) -> anyhow::Result<()> {
        self.refuse_evidence_transfer()?;
        let memories = self.store.scroll_all_memories().await?;
        anyhow::ensure!(memories.iter().all(|m| m.evidence_refs.is_empty()), "evidence references present: paired SQLite state required; logical transfer unsupported");
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn store_with_evidence(
        &self,
        content: &str,
        tags: Vec<String>,
        realm: Option<&str>,
        source: Option<&str>,
        visibility: Option<&str>,
        key: Option<&str>,
        capture: &CaptureRequest,
    ) -> Result<String, ApiError> {
        let _guard = self.store.maintenance_guard().await;
        if !self.config.evidence.enabled {
            return Err(ApiError::BadRequest("evidence capture is disabled".into()));
        }
        // Limit this slice to a single well-defined request contract; legacy fields
        // cannot silently change semantics on a retry.
        if realm.is_some() || source.is_some() {
            return Err(ApiError::BadRequest(
                "evidence capture does not support source or realm_hint; use tags for scope".into(),
            ));
        }
        let key =
            key.ok_or_else(|| ApiError::BadRequest("evidence requires idempotency_key".into()))?;
        let ledger = EvidenceStore::open(&self.evidence_path())?;
        let record = ledger
            .prepare(
                key,
                content,
                capture,
                &tags,
                visibility.unwrap_or("private"),
            )
            .map_err(|_| {
                ApiError::BadRequest(
                    "invalid evidence metadata or conflicting idempotency key".into(),
                )
            })?;
        // A durable prepare may survive embedding/Qdrant failure. Retry reuses both
        // IDs, never inserts another memory and never exposes unlinked source text.
        if let Some(memory) = self.store.get_memory(&record.memory_id).await? {
            if memory
                .evidence_refs
                .iter()
                .any(|r| r.source_id == record.source_id)
            {
                return Ok(record.memory_id);
            }
        }
        let vector = self.make_embedder()?.embed(&record.content).await?;
        if vector.len() != self.embed_dimensions as usize || vector.iter().any(|v| !v.is_finite()) {
            return Err(ApiError::Internal(anyhow::anyhow!(
                "invalid embedding dimensions"
            )));
        }
        let realm_id = self.auto_assign_realm(&vector).await?;
        let realm_name = self
            .store
            .get_realm(&realm_id)
            .await?
            .map(|r| r.name)
            .unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();
        let memory = crate::storage::qdrant::MemoryPoint {
            id: record.memory_id.clone(),
            content: record.content.clone(),
            summary: None,
            source_file: None,
            realm_id: Some(realm_id),
            realm_name,
            importance: 1.0,
            upvotes: 0,
            tags: record.scope.clone(),
            ingested_at: now.clone(),
            last_accessed: now,
            access_count: 0,
            chunk_type: "manual".into(),
            heading: None,
            source_hash: record.content_hash.clone(),
            related_memory_ids: vec![],
            association_strengths: vec![],
            reaction_score: 0.0,
            topic_clusters: vec![],
            quantized_bit_width: 0.0,
            last_verified: None,
            verification_confidence: None,
            verification_status: "unverified".into(),
            consolidated_from: vec![],
            derived_by: String::new(),
            consolidated_at: None,
            no_consolidate_until: None,
            consolidated_evidence: vec![],
            no_dedup_until: None,
            dedup_applied_operations: vec![],
            dedup_access_contributions: Default::default(),
            certified_at: None,
            certified_by: None,
            certification_note: None,
            memory_type: "general".into(),
            visibility: record.visibility.clone(),
            evidence_refs: vec![EvidenceRef {
                schema_version: 1,
                source_id: record.source_id.clone(),
                start_byte: 0,
                end_byte: record.content.len(),
            }],
        };
        let lexical_text = crate::engine::lexical::memory_text(&memory);
        // A captured memory is a new corpus member; a loaded index built before
        // this point would hide it. Invalidate before the projection write, the
        // same way `store_memory` does.
        self.invalidate_compressed_index().await;
        self.store
            .restore_memory(&crate::storage::qdrant::MemoryWithVector { memory, vector })
            .await?;
        self.lexical
            .write()
            .await
            .upsert(&record.memory_id, &lexical_text);
        Ok(record.memory_id)
    }
    pub async fn read_evidence(
        &self,
        memory_id: &str,
        source_id: &str,
    ) -> Result<SourceRecord, ApiError> {
        let _read = self.store.maintenance_read_guard().await;
        if uuid::Uuid::parse_str(memory_id).is_err() || uuid::Uuid::parse_str(source_id).is_err() {
            return Err(ApiError::BadRequest("expected UUID identifiers".into()));
        }
        let not_found = || ApiError::NotFound("evidence".into());
        if self.erased_ids()?.contains(memory_id) {
            return Err(not_found());
        }
        let memory = self
            .store
            .get_memory(memory_id)
            .await?
            .ok_or_else(not_found)?;
        let link = memory
            .evidence_refs
            .iter()
            .find(|r| r.source_id == source_id)
            .ok_or_else(not_found)?;
        let store = self.evidence_store()?.ok_or_else(not_found)?;
        let record = store.get(source_id)?.ok_or_else(not_found)?;
        // Never return source text under another memory's linkage or changed scope.
        if record.memory_id != memory_id
            || record.scope != memory.tags
            || record.visibility != memory.visibility
            || link.schema_version != 1
            || link.start_byte != 0
            || link.end_byte != record.content.len()
        {
            return Err(not_found());
        }
        Ok(record)
    }
}
