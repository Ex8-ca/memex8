use crate::config::AppConfig;
use crate::storage::qdrant::QdrantStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Realm {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub centroid_vector: Vec<f32>,
    pub memory_count: u32,
    pub is_user_pinned: bool,
}

pub struct RealmEngine {
    config: AppConfig,
    store: QdrantStore,
}

impl RealmEngine {
    pub fn new(config: AppConfig, store: QdrantStore) -> Self {
        Self { config, store }
    }

    pub async fn assign_to_realm(&self, vector: &[f32], hint: Option<&str>) -> anyhow::Result<String> {
        // Check hint first
        if let Some(hint_name) = hint {
            if let Some(realm) = self.store.find_realm_by_name(hint_name).await? {
                return Ok(realm.id);
            }
        }

        // Find nearest realm
        let realms = self.store.list_realms().await?;
        let mut best = None;
        let mut best_score = -1.0f32;

        for realm in &realms {
            let score = cosine_similarity(vector, &realm.centroid);
            if score > best_score {
                best_score = score;
                best = Some(realm.clone());
            }
        }

        if let Some(realm) = best {
            if best_score >= self.config.realms.similarity_threshold {
                return Ok(realm.id);
            }
        }

        // Create new auto-discovered realm
        let id = uuid::Uuid::new_v4().to_string();
        let name = format!("auto-{}", &id[..8]);
        self.store.store_realm(&id, vector, &name, None, false).await?;
        tracing::info!("Auto-discovered new realm: {} (score was {:.3})", name, best_score);
        Ok(id)
    }

    pub async fn recompute_centroids(&self) -> anyhow::Result<u32> {
        // TODO: for each realm, fetch all member memories, compute mean vector, update centroid
        tracing::info!("Recomputing realm centroids...");
        Ok(0)
    }

    pub async fn check_split(&self, _realm_id: &str) -> anyhow::Result<bool> {
        // TODO: k-means k=2 on realm members, check if sub-clusters have sufficient distance
        Ok(false)
    }

    pub async fn check_merge(&self) -> anyhow::Result<u32> {
        // TODO: compare all realm centroid pairs, merge those below merge_threshold
        Ok(0)
    }

    pub async fn create_user_realm(&self, name: &str, description: Option<&str>) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        // Embed the description as the initial centroid
        let centroid = vec![0.0f32; self.config.embedding.dimensions as usize]; // TODO: embed description
        self.store.store_realm(&id, &centroid, name, description, true).await?;
        Ok(id)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}
