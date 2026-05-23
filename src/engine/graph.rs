use crate::storage::qdrant::{GraphEdge, QdrantStore};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// ─── Data types ───────────────────────────────────────────────────────────────

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

/// Result of a graph traversal — includes the path taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphTraversalResult {
    pub memory_id: String,
    pub depth: usize,
    pub path: Vec<TraversalStep>,
    pub relevance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalStep {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: String,
    pub weight: f32,
}

/// Statistics about the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub total_edges: usize,
    pub edge_types: HashMap<String, usize>,
    pub unique_entities: usize,
}

// ─── Knowledge Graph ──────────────────────────────────────────────────────────

pub struct KnowledgeGraph {
    store: QdrantStore,
}

impl KnowledgeGraph {
    pub fn new(store: QdrantStore) -> Self {
        Self { store }
    }

    /// Extract entities from text using rule-based NER.
    pub fn extract_entities(text: &str) -> Vec<Entity> {
        let mut entities = Vec::new();
        let mut seen = HashSet::new();

        // Helper: run a regex and process captures
        let mut add_from_re = |re: Result<Regex, _>, get_name: &dyn Fn(&regex::Captures) -> Option<String>, etype: &str| {
            if let Ok(re) = re {
                for cap in re.captures_iter(text) {
                    if let Some(name) = get_name(&cap) {
                        let name = name.trim().to_string();
                        if !name.is_empty() && seen.insert(name.clone()) {
                            entities.push(Entity {
                                id: entity_id(&name, etype),
                                name,
                                entity_type: etype.to_string(),
                            });
                        }
                    }
                }
            }
        };

        // 1. Technical terms in backticks
        add_from_re(
            Regex::new(r"`([^`]+)`"),
            &|cap| cap.get(1).map(|m| m.as_str().to_string()),
            "tech_term",
        );

        // 2. URLs
        add_from_re(
            Regex::new(r#"https?://[^\s<>\[\]{}"',;)]+"#),
            &|cap| cap.get(0).map(|m| m.as_str().to_string()),
            "url",
        );

        // 3. File paths
        add_from_re(
            Regex::new(r"(?:^|\s)([/~][\w./_-]{3,})"),
            &|cap| cap.get(1).map(|m| m.as_str().to_string()),
            "file_path",
        );

        // 4. Config patterns: key: value
        let config_keys = [
            "port", "host", "provider", "model", "version", "path", "url", "name", "type",
            "api_key", "token", "key", "value", "timeout", "retries", "limit", "size", "mode",
            "backend", "endpoint", "region", "bucket",
        ];
        let config_pattern = format!(r"(?:^|\s)({}):[ \t]*([^\s,;]+)", config_keys.join("|"));
        add_from_re(
            Regex::new(&config_pattern),
            &|cap| {
                let k = cap.get(1)?.as_str();
                let v = cap.get(2)?.as_str().trim();
                Some(format!("{}:{}", k, v))
            },
            "config",
        );

        // 5. Acronyms
        add_from_re(
            Regex::new(r"\b([A-Z]{2,5})\b"),
            &|cap| cap.get(1).map(|m| m.as_str().to_string()),
            "acronym",
        );

        // 6. Capitalized sequences (multi-word)
        add_from_re(
            Regex::new(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)+)\b"),
            &|cap| {
                let name = cap.get(1)?.as_str();
                if name.len() >= 5 {
                    Some(name.to_string())
                } else {
                    None
                }
            },
            "person", // Will be re-classified below
        );

        // Re-classify multi-word entities
        for entity in entities.iter_mut() {
            if entity.entity_type == "person" && entity.name.split_whitespace().count() >= 2 {
                entity.entity_type = classify_proper_noun(&entity.name);
            }
        }

        // 7. Single capitalized words (conservative)
        let common_starters: HashSet<&str> = [
            "The", "This", "That", "These", "Those", "There", "Here", "When", "Where", "What",
            "Which", "Who", "Why", "How", "If", "But", "And", "For", "Not", "Now", "Then",
            "All", "Each", "Every", "Some", "Any", "No", "Many", "Much", "More", "Most",
            "Other", "Another", "Such", "Only", "Just", "Also", "Very", "Still", "Even",
            "However", "Therefore", "Moreover", "Furthermore", "Additionally", "Consequently",
            "Meanwhile", "Otherwise", "Instead", "Rather", "Yet", "So", "Or", "Nor",
            "A", "An", "In", "On", "At", "To", "Of", "By", "With", "From", "Up", "About",
            "Into", "Over", "After", "Before", "Between", "Under", "Through", "During",
            "Without", "Until", "Within", "Across", "Behind", "Beyond", "Around",
            "It", "Its", "Is", "Are", "Was", "Were", "Be", "Been", "Being", "Have", "Has", "Had",
            "Do", "Does", "Did", "Will", "Would", "Could", "Should", "May", "Might", "Must",
            "Can", "Shall", "Need", "Dare", "Used", "Ought",
        ].into_iter().collect();

        if let Ok(re) = Regex::new(r"\b([A-Z][a-z]{2,})\b") {
            for cap in re.captures_iter(text) {
                if let Some(m) = cap.get(1) {
                    let name = m.as_str();
                    if !common_starters.contains(name) && seen.insert(name.to_string()) {
                        if looks_like_proper_noun(name) {
                            entities.push(Entity {
                                id: entity_id(name, "person"),
                                name: name.to_string(),
                                entity_type: "person".to_string(),
                            });
                        }
                    }
                }
            }
        }

        entities
    }

    /// Build the knowledge graph from all memories stored in Qdrant.
    pub async fn build_graph(&self, similarity_threshold: f32) -> anyhow::Result<usize> {
        let memories = self.store.scroll_all_memories_with_vectors().await?;
        tracing::info!(
            "Building knowledge graph from {} memories...",
            memories.len()
        );

        if memories.is_empty() {
            return Ok(0);
        }

        // Phase 1: Extract entities for each memory
        let mut memory_entities: HashMap<String, HashSet<String>> = HashMap::new();
        for m in &memories {
            let entities = Self::extract_entities(&m.memory.content);
            let entity_names: HashSet<String> = entities.into_iter().map(|e| e.name).collect();
            memory_entities.insert(m.memory.id.clone(), entity_names);
        }

        // Phase 2: Build entity -> memory_ids index
        let mut entity_to_memories: HashMap<String, Vec<String>> = HashMap::new();
        for (mem_id, entities) in &memory_entities {
            for entity_name in entities {
                entity_to_memories
                    .entry(entity_name.clone())
                    .or_default()
                    .push(mem_id.clone());
            }
        }

        // Phase 3: Create "co_occurs" edges for memories sharing entities
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut seen_pairs: HashSet<(String, String)> = HashSet::new();

        for (_entity, mem_ids) in &entity_to_memories {
            if mem_ids.len() < 2 {
                continue;
            }
            let inverse_freq = 1.0 / mem_ids.len() as f32;

            for i in 0..mem_ids.len() {
                for j in (i + 1)..mem_ids.len() {
                    let pair = order_pair(&mem_ids[i], &mem_ids[j]);
                    if seen_pairs.insert(pair.clone()) {
                        let weight = (inverse_freq * 2.0).min(1.0);
                        edges.push(GraphEdge {
                            from_memory_id: pair.0,
                            to_memory_id: pair.1,
                            relation_type: "co_occurs".to_string(),
                            weight,
                        });
                    }
                }
            }
        }

        // Phase 4: Create "similar" edges via vector similarity
        for m in &memories {
            if m.vector.is_empty() {
                continue;
            }
            let similar = self.store.find_similar(&m.memory.id, 5).await?;
            for (other_id, sim_score) in &similar {
                if *sim_score >= similarity_threshold {
                    let pair = order_pair(&m.memory.id, other_id);
                    if seen_pairs.insert(pair.clone()) {
                        edges.push(GraphEdge {
                            from_memory_id: pair.0,
                            to_memory_id: pair.1,
                            relation_type: "similar".to_string(),
                            weight: *sim_score,
                        });
                    }
                }
            }
        }

        // Phase 5: Store edges in Qdrant
        let edge_count = edges.len();
        for edge in &edges {
            if let Err(e) = self.store.store_graph_edge(edge).await {
                tracing::warn!("Failed to store graph edge: {}", e);
            }
        }

        tracing::info!("Knowledge graph built: {} edges created", edge_count);
        Ok(edge_count)
    }

    /// BFS traversal from a starting memory_id up to `depth` levels.
    pub async fn search_graph(
        &self,
        memory_id: &str,
        depth: usize,
    ) -> anyhow::Result<Vec<GraphTraversalResult>> {
        if depth == 0 {
            return Ok(vec![]);
        }

        let all_edges = self.store.get_all_graph_edges().await?;

        // Build adjacency list
        let mut adj: HashMap<String, Vec<&GraphEdge>> = HashMap::new();
        for edge in &all_edges {
            adj.entry(edge.from_memory_id.clone())
                .or_default()
                .push(edge);
            adj.entry(edge.to_memory_id.clone())
                .or_default()
                .push(edge);
        }

        // BFS
        let mut visited: HashSet<String> = HashSet::new();
        let mut results: Vec<GraphTraversalResult> = Vec::new();
        let mut queue: VecDeque<(String, Vec<TraversalStep>)> = VecDeque::new();

        visited.insert(memory_id.to_string());
        queue.push_back((memory_id.to_string(), vec![]));

        while let Some((current, path)) = queue.pop_front() {
            let current_depth = path.len();
            if current_depth >= depth {
                continue;
            }

            if let Some(neighbors) = adj.get(&current) {
                for edge in neighbors {
                    let next_id = if edge.from_memory_id == current {
                        &edge.to_memory_id
                    } else {
                        &edge.from_memory_id
                    };

                    if visited.contains(next_id.as_str()) {
                        continue;
                    }
                    visited.insert(next_id.clone());

                    let mut new_path = path.clone();
                    new_path.push(TraversalStep {
                        from_id: current.clone(),
                        to_id: next_id.clone(),
                        relation_type: edge.relation_type.clone(),
                        weight: edge.weight,
                    });

                    if let Some(_memory) = self.store.get_memory(next_id).await? {
                        let relevance = new_path.iter().map(|s| s.weight).sum::<f32>()
                            / new_path.len() as f32;
                        results.push(GraphTraversalResult {
                            memory_id: next_id.clone(),
                            depth: new_path.len(),
                            path: new_path.clone(),
                            relevance,
                        });
                    }

                    queue.push_back((next_id.clone(), new_path));
                }
            }
        }

        results.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }

    /// Get directly connected entities/memories for a given memory_id.
    pub async fn get_neighbors(&self, memory_id: &str) -> anyhow::Result<Vec<Relationship>> {
        let edges = self.store.get_graph_edges_for_memory(memory_id).await?;
        Ok(edges
            .into_iter()
            .map(|e| Relationship {
                from_id: e.from_memory_id,
                to_id: e.to_memory_id,
                relation_type: e.relation_type,
                weight: e.weight,
            })
            .collect())
    }

    /// Get statistics about the knowledge graph.
    pub async fn get_stats(&self) -> anyhow::Result<GraphStats> {
        let edges = self.store.get_all_graph_edges().await?;

        let mut edge_types: HashMap<String, usize> = HashMap::new();
        let mut unique_entities: HashSet<String> = HashSet::new();

        for edge in &edges {
            *edge_types.entry(edge.relation_type.clone()).or_insert(0) += 1;
            unique_entities.insert(edge.from_memory_id.clone());
            unique_entities.insert(edge.to_memory_id.clone());
        }

        Ok(GraphStats {
            total_edges: edges.len(),
            edge_types,
            unique_entities: unique_entities.len(),
        })
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────

fn entity_id(name: &str, entity_type: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    entity_type.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn classify_proper_noun(name: &str) -> String {
    let lower = name.to_lowercase();

    let product_indicators = [
        "api", "sdk", "cli", "gui", "ide", "os", "app", "bot", "tool", "service",
        "platform", "framework", "library", "engine", "system", "server", "client",
        "cloud", "net", "web", "db", "ai", "ml", "gpu", "cpu",
    ];
    for indicator in &product_indicators {
        if lower.contains(indicator) {
            return "product".to_string();
        }
    }

    let place_indicators = [
        "street", "avenue", "road", "boulevard", "city", "state", "country",
        "university", "college", "park", "center", "plaza",
    ];
    for indicator in &place_indicators {
        if lower.contains(indicator) {
            return "place".to_string();
        }
    }

    if name.split_whitespace().count() >= 3 {
        return "product".to_string();
    }

    "person".to_string()
}

fn looks_like_proper_noun(word: &str) -> bool {
    let common_nouns: HashSet<&str> = [
        "The", "This", "That", "There", "Here", "When", "Where", "What", "Which",
        "One", "Two", "Three", "First", "Second", "Third", "Last", "Next", "Previous",
        "New", "Old", "Good", "Best", "Better", "Great", "Small", "Large", "Long",
        "High", "Low", "Big", "Little", "Important", "Main", "Key", "Basic", "Simple",
        "Complex", "Full", "Empty", "True", "False", "Right", "Left", "Top", "Bottom",
        "Front", "Back", "Side", "Part", "Way", "Time", "Day", "Year", "Week", "Month",
        "Use", "Used", "Using", "Make", "Made", "Find", "Found", "Set", "Get", "Go",
        "Run", "Work", "Call", "Try", "Ask", "Need", "Take", "Give", "Come", "Know",
        "Think", "See", "Look", "Want", "Put", "Help", "Show", "Start", "End", "Turn",
    ].into_iter().collect();

    !common_nouns.contains(word) && word.len() >= 3
}

fn order_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_entities_technical_terms() {
        let text = "Use `Rust` and `tokio` for async programming";
        let entities = KnowledgeGraph::extract_entities(text);
        let tech_terms: Vec<_> = entities.iter().filter(|e| e.entity_type == "tech_term").collect();
        assert_eq!(tech_terms.len(), 2);
        assert!(tech_terms.iter().any(|e| e.name == "Rust"));
        assert!(tech_terms.iter().any(|e| e.name == "tokio"));
    }

    #[test]
    fn test_extract_entities_urls() {
        let text = "Visit https://example.com or http://test.org/path for details";
        let entities = KnowledgeGraph::extract_entities(text);
        let urls: Vec<_> = entities.iter().filter(|e| e.entity_type == "url").collect();
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn test_extract_entities_acronyms() {
        let text = "The API uses JWT auth with OAuth2 and HTTP";
        let entities = KnowledgeGraph::extract_entities(text);
        let acronyms: Vec<_> = entities.iter().filter(|e| e.entity_type == "acronym").collect();
        assert!(acronyms.iter().any(|e| e.name == "API"));
        assert!(acronyms.iter().any(|e| e.name == "JWT"));
    }

    #[test]
    fn test_extract_entities_config() {
        let text = "Set port: 8080 and provider: ollama for the server";
        let entities = KnowledgeGraph::extract_entities(text);
        let configs: Vec<_> = entities.iter().filter(|e| e.entity_type == "config").collect();
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().any(|e| e.name == "port:8080"));
        assert!(configs.iter().any(|e| e.name == "provider:ollama"));
    }

    #[test]
    fn test_extract_entities_capitalized() {
        let text = "John Smith works at Google Inc in New York";
        let entities = KnowledgeGraph::extract_entities(text);
        // Should find at least the multi-word entities
        let multi_word: Vec<_> = entities.iter()
            .filter(|e| e.name.split_whitespace().count() >= 2)
            .collect();
        assert!(multi_word.iter().any(|e| e.name == "John Smith"));
        assert!(multi_word.iter().any(|e| e.name == "New York"));
    }

    #[test]
    fn test_entity_id_deterministic() {
        let id1 = entity_id("Rust", "tech_term");
        let id2 = entity_id("Rust", "tech_term");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_order_pair() {
        let (a, b) = order_pair("zzz", "aaa");
        assert_eq!(a, "aaa");
        assert_eq!(b, "zzz");
    }
}
