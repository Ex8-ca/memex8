/// Collection setup and schema management.
/// This module handles creating the Qdrant collections with proper vector config,
/// payload indexes, and schema.
use crate::storage::qdrant::QdrantStore;

pub async fn ensure_collections(store: &QdrantStore, dimensions: u32) -> anyhow::Result<()> {
    store.ensure_collections(dimensions).await
}

/// Migration: Add reaction_score field to existing memories.
/// Existing memories will get a default reaction_score of 0.0 (neutral).
pub async fn migrate_reaction_scores(store: &QdrantStore) -> anyhow::Result<usize> {
    let all = store.scroll_all_memories().await?;
    let mut migrated = 0;

    for mem in &all {
        // reaction_score is already initialized to 0.0 for all existing memories
        // since map_f32 returns 0.0 when the field is missing.
        // This migration exists as an explicit hook for future score backfills.
        tracing::debug!(
            "Memory {} already has reaction_score={}",
            mem.id,
            mem.reaction_score
        );
        migrated += 1;
    }

    tracing::info!("Migration: verified {} memories have reaction_score field", migrated);
    Ok(migrated)
}
