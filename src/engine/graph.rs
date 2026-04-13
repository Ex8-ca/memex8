use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: String,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReference {
    pub memory_id: String,
    pub relevance: f32,
}

pub struct KnowledgeGraph;

impl KnowledgeGraph {
    pub fn new() -> Self {
        Self
    }

    /// Extract entities from text using rule-based NER
    pub fn extract_entities(&self, text: &str) -> Vec<Entity> {
        // TODO: implement named entity recognition
        // - Capitalized sequences (names, places, products)
        // - Technical terms in backticks
        // - URLs and file paths
        // - Acronyms
        vec![]
    }

    pub async fn add_relationship(
        &self,
        from: &str,
        to: &str,
        relation: &str,
    ) -> anyhow::Result<()> {
        // TODO: store in Qdrant payload or separate graph store
        Ok(())
    }

    pub async fn get_neighbors(&self, entity: &str) -> anyhow::Result<Vec<Relationship>> {
        // TODO: query graph store
        Ok(vec![])
    }

    pub async fn search_graph(
        &self,
        entity: &str,
        depth: usize,
    ) -> anyhow::Result<Vec<MemoryReference>> {
        // TODO: BFS/DFS from entity, return connected memories
        Ok(vec![])
    }
}
