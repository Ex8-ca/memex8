//! Durable consolidation journal. Entries are retained after undo for retries/audit.
use super::*;

const ARCHIVE: &str = "consolidation_archive";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationOperation {
    pub version: u32,
    #[serde(default = "consolidation_kind")]
    pub kind: String,
    /// Dedup journals retain the winner before the merge for audit; never overwrite it on restore.
    #[serde(default)]
    pub dedup_winner: Option<MemoryPoint>,
    pub summary_id: String,
    pub summary_hash: String,
    pub created_at: String,
    /// prepared -> complete -> restoring -> restored; errors retain the last durable state.
    pub state: String,
    pub preserve_originals: bool,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub sources: Vec<MemoryWithVector>,
    pub cooldown_until: Option<String>,
}

fn consolidation_kind() -> String {
    "consolidation".into()
}

#[derive(Debug)]
pub enum RecoveryError {
    NotFound(String),
    Conflict(String),
}
impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Conflict(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for RecoveryError {}

#[derive(Debug, Serialize)]
pub struct UnconsolidateResult {
    pub summary_id: String,
    pub source_ids: Vec<String>,
    pub restored: usize,
    pub retained: usize,
    pub already_restored: bool,
    pub no_consolidate_until: Option<String>,
}

pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

// Legacy payloads may omit fields that deserialize to empty strings.
fn snapshot_condition(field: &str, value: &str) -> Condition {
    if value.is_empty() {
        Filter::should([
            Condition::matches(field, String::new()),
            Condition::is_empty(field),
        ])
        .into()
    } else {
        Condition::matches(field, value.to_string())
    }
}

impl QdrantStore {
    pub(super) async fn ensure_consolidation_archive(&self) -> anyhow::Result<()> {
        if !self.client.collection_exists(ARCHIVE).await? {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(ARCHIVE)
                        .vectors_config(VectorParamsBuilder::new(1, Distance::Dot))
                        .on_disk_payload(true),
                )
                .await?;
        }
        for field in ["operation.kind", "operation.state"] {
            self.client
                .create_field_index(
                    CreateFieldIndexCollectionBuilder::new(ARCHIVE, field, FieldType::Keyword)
                        .wait(true),
                )
                .await?;
        }
        Ok(())
    }

    pub async fn get_memory_snapshot(&self, id: &str) -> anyhow::Result<Option<MemoryWithVector>> {
        let response = self
            .client
            .get_points(
                GetPointsBuilder::new(MEMORIES, vec![id.into()])
                    .with_payload(true)
                    .with_vectors(true),
            )
            .await?;
        let Some(point) = response.result.into_iter().next() else {
            return Ok(None);
        };
        let vector = extract_vector(&point)
            .ok_or_else(|| anyhow::anyhow!("Memory {} has no restorable dense vector", id))?;
        anyhow::ensure!(
            !vector.is_empty() && vector.iter().all(|v| v.is_finite()),
            "Memory {} has an invalid vector",
            id
        );
        Ok(Some(MemoryWithVector {
            memory: memory_from_payload(id, &map_to_json(&point.payload)),
            vector,
        }))
    }

    pub async fn get_consolidation(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<ConsolidationOperation>> {
        let response = self
            .client
            .get_points(GetPointsBuilder::new(ARCHIVE, vec![id.into()]).with_payload(true))
            .await?;
        response
            .result
            .into_iter()
            .next()
            .map(|point| {
                let map = map_to_json(&point.payload);
                let operation: ConsolidationOperation = serde_json::from_value(
                    map.get("operation")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Invalid consolidation journal"))?,
                )?;
                anyhow::ensure!(
                    operation.version == 1 && operation.summary_id == id,
                    "Unsupported or mismatched consolidation journal"
                );
                Ok(operation)
            })
            .transpose()
    }

    pub async fn list_consolidations(&self) -> anyhow::Result<Vec<ConsolidationOperation>> {
        self.list_consolidations_filtered(None).await
    }

    pub async fn pending_dedup_operations(&self) -> anyhow::Result<Vec<ConsolidationOperation>> {
        self.list_consolidations_filtered(Some(Filter::must([
            Condition::matches("operation.kind", "dedup".to_string()),
            Condition::matches("operation.state", "prepared".to_string()),
        ])))
        .await
    }

    async fn list_consolidations_filtered(
        &self,
        filter: Option<Filter>,
    ) -> anyhow::Result<Vec<ConsolidationOperation>> {
        let mut operations = Vec::new();
        let mut offset = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(ARCHIVE)
                .limit(100)
                .with_payload(true);
            if let Some(filter) = &filter {
                builder = builder.filter(filter.clone());
            }
            if let Some(value) = offset {
                builder = builder.offset(value);
            }
            let response = self.client.scroll(builder).await?;
            for point in response.result {
                let id = point_id_to_string(point.id.as_ref());
                let map = map_to_json(&point.payload);
                let operation: ConsolidationOperation = serde_json::from_value(
                    map.get("operation")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Invalid consolidation journal"))?,
                )?;
                anyhow::ensure!(
                    operation.version == 1 && operation.summary_id == id,
                    "Unsupported or mismatched consolidation journal"
                );
                operations.push(operation);
            }
            offset = response.next_page_offset;
            if offset.is_none() {
                break;
            }
        }
        Ok(operations)
    }

    /// Persist and read back the complete journal before callers may remove sources.
    pub async fn archive_consolidation(
        &self,
        operation: &ConsolidationOperation,
    ) -> anyhow::Result<()> {
        crate::engine::redaction::ensure_clean(operation)?;
        anyhow::ensure!(
            self.get_consolidation(&operation.summary_id)
                .await?
                .is_none(),
            "Consolidation journal already exists"
        );
        let payload: Payload = serde_json::json!({"operation": operation}).try_into()?;
        self.client
            .upsert_points(
                UpsertPointsBuilder::new(
                    ARCHIVE,
                    vec![PointStruct::new(
                        operation.summary_id.clone(),
                        vec![1.0],
                        payload,
                    )],
                )
                .wait(true),
            )
            .await?;
        let saved = self
            .get_consolidation(&operation.summary_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Consolidation archive read-back failed"))?;
        anyhow::ensure!(
            serde_json::to_value(&saved)? == serde_json::to_value(operation)?,
            "Consolidation archive read-back mismatch"
        );
        Ok(())
    }

    pub async fn update_consolidation_state(
        &self,
        operation: &mut ConsolidationOperation,
        state: &str,
    ) -> anyhow::Result<()> {
        crate::engine::redaction::ensure_clean(operation)?;
        operation.state = state.into();
        let payload: Payload = serde_json::json!({"operation": operation}).try_into()?;
        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(ARCHIVE, payload)
                    .points_selector(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: vec![operation.summary_id.clone().into()],
                    }))
                    .wait(true),
            )
            .await?;
        let saved = self
            .get_consolidation(&operation.summary_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Consolidation journal missing"))?;
        anyhow::ensure!(
            saved.state == state && saved.cooldown_until == operation.cooldown_until,
            "Consolidation journal state read-back mismatch"
        );
        Ok(())
    }

    /// Remove dangling graph/association references; rerunning after a partial
    /// failure is harmless. Realm counts and compressed indexes rebuild in slumber.
    pub async fn prune_duplicate_references(&self, id: &str) -> anyhow::Result<()> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(GRAPHS)
                    .points(PointsSelectorOneOf::Filter(Filter::should([
                        Condition::matches("from_memory_id", id.to_string()),
                        Condition::matches("to_memory_id", id.to_string()),
                    ])))
                    .wait(true),
            )
            .await?;
        // Collect matching records before changing the indexed field so pagination
        // remains stable while references are removed.
        let mut affected = Vec::new();
        let mut offset = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(MEMORIES)
                .filter(Filter::must([Condition::matches(
                    "related_memory_ids",
                    id.to_string(),
                )]))
                .limit(100)
                .with_payload(true);
            if let Some(value) = offset {
                builder = builder.offset(value);
            }
            let response = self.client.scroll(builder).await?;
            for point in response.result {
                let memory_id = point_id_to_string(point.id.as_ref());
                affected.push(memory_from_payload(
                    &memory_id,
                    &map_to_json(&point.payload),
                ));
            }
            offset = response.next_page_offset;
            if offset.is_none() {
                break;
            }
        }
        for memory in affected {
            let mut ids = Vec::new();
            let mut strengths = Vec::new();
            for (i, related) in memory.related_memory_ids.iter().enumerate() {
                if related == id {
                    continue;
                }
                ids.push(related.clone());
                strengths.push(memory.association_strengths.get(i).copied().unwrap_or(0.0));
            }
            self.update_memory_payload(
                &memory.id,
                serde_json::json!({
                    "related_memory_ids": ids, "association_strengths": strengths,
                })
                .try_into()?,
            )
            .await?;
        }
        Ok(())
    }

    /// Apply dedup metadata and the idempotency marker in one Qdrant payload write.
    pub async fn apply_dedup_payload(
        &self,
        expected: &MemoryPoint,
        payload: Payload,
        operation_id: &str,
    ) -> anyhow::Result<()> {
        let filter = Filter::must([
            Condition::has_id([expected.id.clone()]),
            Condition::matches("content", expected.content.clone()),
            snapshot_condition("source_hash", &expected.source_hash),
            snapshot_condition("memory_type", &expected.memory_type),
            Condition::matches("visibility", expected.visibility.clone()),
        ]);
        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(MEMORIES, payload)
                    .points_selector(PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await?;
        let memory = self
            .get_memory(&expected.id)
            .await?
            .ok_or_else(|| RecoveryError::Conflict("Duplicate winner disappeared".into()))?;
        if !memory
            .dedup_applied_operations
            .iter()
            .any(|id| id == operation_id)
        {
            return Err(RecoveryError::Conflict(
                "Duplicate winner changed before metadata merge".into(),
            )
            .into());
        }
        Ok(())
    }

    /// Match content/type/visibility and certification at the actual deletion, so
    /// an edited or newly certified source cannot be removed from a stale snapshot.
    pub async fn delete_consolidation_snapshot(&self, memory: &MemoryPoint) -> anyhow::Result<()> {
        let filter = Filter {
            must: vec![
                Condition::has_id([memory.id.clone()]),
                Condition::matches("content", memory.content.clone()),
                snapshot_condition("source_hash", &memory.source_hash),
                Condition::matches("visibility", memory.visibility.clone()),
                snapshot_condition("memory_type", &memory.memory_type),
            ],
            must_not: vec![Condition::matches(
                "verification_status",
                "certified".to_string(),
            )],
            ..Default::default()
        };
        self.client
            .delete_points(
                DeletePointsBuilder::new(MEMORIES)
                    .points(PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await?;
        if self.get_memory(&memory.id).await?.is_some() {
            return Err(RecoveryError::Conflict(format!(
                "Memory {} changed or is certified; it was retained",
                memory.id,
            ))
            .into());
        }
        Ok(())
    }
}
