/// Collection setup and schema management.
/// This module handles creating the Qdrant collections with proper vector config,
/// payload indexes, and schema.
use crate::storage::qdrant::QdrantStore;

pub async fn ensure_collections(store: &QdrantStore, dimensions: u32) -> anyhow::Result<()> {
    store.ensure_collections(dimensions).await
}
