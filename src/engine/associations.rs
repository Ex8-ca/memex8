//! Phase 2: Semantic Associations & Gap Detection
//! Detects topic clusters, infers relationship types, and identifies knowledge gaps.

use crate::storage::qdrant::MemoryPoint;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Topic cluster identifier and its member memory IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicCluster {
    /// Unique cluster ID.
    pub id: String,
    /// Human-readable cluster label (auto-generated).
    pub label: String,
    /// Memory IDs belonging to this cluster.
    pub memory_ids: Vec<String>,
    /// Cluster centroid (mean of member vectors) — stored as f32 array for serialization.
    #[serde(default)]
    pub centroid: Vec<f32>,
    /// How strongly this cluster connects to others (sum of link strengths).
    #[serde(default)]
    pub total_link_strength: f32,
}

/// Types of inferred semantic links between memories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LinkType {
    /// Memory B fills a conceptual gap between A and C.
    FillGap,
    /// Memory B comes temporally after A (chronological sequence).
    TemporalNext,
    /// Memory B is a prerequisite for understanding A.
    Prerequisite,
    /// Memory B is a companion/frequently co-occurring with A.
    Companion,
    /// A and B are in the same topic cluster (implicit).
    SameCluster,
    /// No clear relationship.
    None,
}

impl LinkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkType::FillGap => "FillGap",
            LinkType::TemporalNext => "TemporalNext",
            LinkType::Prerequisite => "Prerequisite",
            LinkType::Companion => "Companion",
            LinkType::SameCluster => "SameCluster",
            LinkType::None => "None",
        }
    }
}

/// An inferred semantic link between two memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredLink {
    /// Source memory ID.
    pub from_id: String,
    /// Target memory ID.
    pub to_id: String,
    /// Classification of the relationship.
    pub link_type: LinkType,
    /// Confidence score [0.0, 1.0].
    pub confidence: f32,
    /// Human-readable explanation of why this link was inferred.
    pub explanation: String,
}

/// A detected knowledge gap between topic clusters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    /// Unique gap ID.
    pub id: String,
    /// The source memory or cluster this gap relates to.
    pub context_memory_id: Option<String>,
    /// The source cluster.
    pub from_cluster: String,
    /// The target cluster.
    pub to_cluster: String,
    /// What kind of link is missing (hint for resolution).
    pub missing_link_type: LinkType,
    /// Natural language description of the gap.
    pub description: String,
    /// How confident we are this is a real gap [0.0, 1.0].
    pub confidence: f32,
    /// Whether this gap has been resolved (user filled it).
    #[serde(default)]
    pub resolved: bool,
    /// Memory ID that resolved this gap (if resolved).
    #[serde(default)]
    pub resolution_memory_id: Option<String>,
    /// When the gap was detected.
    #[serde(default)]
    pub detected_at: String,
    /// When the gap was resolved (if resolved).
    #[serde(default)]
    pub resolved_at: Option<String>,
}

impl Gap {
    pub fn new(
        from_cluster: String,
        to_cluster: String,
        missing_link_type: LinkType,
        description: String,
        confidence: f32,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            context_memory_id: None,
            from_cluster,
            to_cluster,
            missing_link_type,
            description,
            confidence,
            resolved: false,
            resolution_memory_id: None,
            detected_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
        }
    }
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).take(n).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// K-means clustering for topic detection.
/// Returns cluster assignments and centroids.
pub fn kmeans_topic_clusters(
    vectors: &[Vec<f32>],
    memory_ids: &[String],
    k: usize,
    max_iter: usize,
) -> (Vec<Vec<f32>>, Vec<i32>) {
    if vectors.is_empty() || k == 0 {
        return (vec![], vec![]);
    }

    let n = vectors.len();
    let dims = vectors[0].len();
    let k = k.min(n);

    // Initialize centroids using k-means++ style seeding
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);

    // Pick first centroid at random
    let first_idx = rand::distributions::Uniform::new(0.0, 1.0)
        .sample_iter(rand::thread_rng())
        .take(1)
        .enumerate()
        .find(|(_, r)| *r > 0.0)
        .map(|(i, _)| i % n)
        .unwrap_or(0);
    centroids.push(vectors[first_idx].clone());

    // Pick remaining k-1 centroids with probability proportional to distance squared
    for _ in 1..k {
        let mut best_d2 = -1.0f32;
        let mut best_idx = 0;
        for (i, v) in vectors.iter().enumerate() {
            let min_d2: f32 = centroids
                .iter()
                .map(|c| {
                    let d = 1.0 - cosine_similarity(v, c);
                    d * d
                })
                .fold(f32::MAX, f32::min);
            if min_d2 > best_d2 || best_d2 < 0.0 {
                best_d2 = min_d2;
                best_idx = i;
            }
        }
        centroids.push(vectors[best_idx].clone());
    }

    // Cluster assignments: which centroid each point belongs to
    let mut assignments = vec![0i32; n];

    for _iter in 0..max_iter {
        let mut changed = false;

        // Assign each vector to nearest centroid
        for (i, v) in vectors.iter().enumerate() {
            let mut best_centroid = 0;
            let mut best_sim = -1.0f32;
            for (j, c) in centroids.iter().enumerate() {
                let sim = cosine_similarity(v, c);
                if sim > best_sim {
                    best_sim = sim;
                    best_centroid = j as i32;
                }
            }
            if assignments[i] != best_centroid {
                changed = true;
            }
            assignments[i] = best_centroid;
        }

        if !changed {
            break;
        }

        // Recompute centroids
        let mut sums = vec![vec![0.0f32; dims]; k];
        let mut counts = vec![0usize; k];

        for (i, v) in vectors.iter().enumerate() {
            let cid = assignments[i] as usize;
            for (j, x) in v.iter().enumerate() {
                sums[cid][j] += x;
            }
            counts[cid] += 1;
        }

        for (j, c) in centroids.iter_mut().enumerate() {
            if counts[j] > 0 {
                for x in c.iter_mut() {
                    *x = 0.0;
                }
                for x in sums[j].iter() {
                    *c.iter_mut().nth(j).unwrap_or(&mut 0.0) += x / counts[j] as f32;
                }
            }
        }

        for (j, c) in centroids.iter_mut().enumerate() {
            if counts[j] > 0 {
                for x in c.iter_mut() {
                    *x /= counts[j] as f32;
                }
            }
        }
    }

    // Fallback: recompute centroids cleanly one more time
    let mut sums = vec![vec![0.0f32; dims]; k];
    let mut counts = vec![0usize; k];
    for (i, v) in vectors.iter().enumerate() {
        let cid = assignments[i] as usize;
        for (j, x) in v.iter().enumerate() {
            sums[cid][j] += x;
        }
        counts[cid] += 1;
    }
    for (j, c) in centroids.iter_mut().enumerate() {
        if counts[j] > 0 {
            for (d, x) in c.iter_mut().enumerate() {
                *x = sums[j][d] / counts[j] as f32;
            }
        }
    }

    tracing::debug!(
        "kmeans: k={} n={} iters={} counts={:?}",
        k,
        n,
        max_iter,
        counts
    );

    (centroids, assignments)
}

/// Auto-generate a cluster label from member memory contents.
/// Uses word frequency to pick the top 2-3 distinctive words.
pub fn generate_cluster_label(memory_ids: &[String], all_memories: &[MemoryPoint]) -> String {
    // Collect texts from all memories in this cluster
    let texts: Vec<&str> = memory_ids
        .iter()
        .filter_map(|id| all_memories.iter().find(|m| &m.id == id))
        .map(|m| m.content.as_str())
        .collect();

    if texts.is_empty() {
        return format!("Cluster-{}", &uuid::Uuid::new_v4().to_string()[..6]);
    }

    let stopwords: std::collections::HashSet<&str> = [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with",
        "by", "from", "is", "are", "was", "were", "be", "been", "being", "have", "has",
        "had", "do", "does", "did", "will", "would", "could", "should", "may", "might",
        "that", "this", "these", "those", "it", "its", "i", "me", "my", "we", "our",
        "you", "your", "he", "him", "his", "she", "her", "they", "them", "their",
        "what", "which", "who", "when", "where", "why", "how", "not", "no", "yes",
        "so", "if", "then", "than", "too", "very", "just", "about", "also", "all",
        "any", "as", "into", "like", "more", "most", "only", "other", "some", "such",
        "up", "down", "after", "before", "between", "through", "during", "below",
        "above", "here", "there", "once", "while", "until", "unless", "because",
        "since", "even", "well", "back", "still", "already", "much", "many", "new",
        "use", "used", "using", "get", "got", "make", "made", "one", "two", "first",
        "last", "next", "each", "every", "both", "few", "way", "thing", "things",
        "work", "want", "need", "know", "think", "see", "come", "go", "take", "give",
        "tell", "say", "said", "told", "help", "run", "went", "going", "set", "show",
        "find", "call", "try", "ask", "put", "keep", "let", "begin", "seem", "leave",
        "turn", "end", "right", "left", "old", "big", "small", "good", "bad", "high",
        "low", "long", "short", "done", "file", "files", "directory", "folder",
        "path", "src", "lib", "build", "target", "install", "installed", "installing",
        "running", "start", "started", "stop", "stopped", "restart", "deploy",
        "deployed", "config", "configuration", "settings", "setup", "server", "client",
        "api", "endpoint", "url", "http", "https", "localhost", "port", "host",
        "app", "application", "project", "code", "coding", "program", "programming",
        "software", "system", "service", "function", "method", "class", "object",
        "type", "string", "number", "int", "float", "array", "list", "map", "vec",
        "vector", "embedding", "model", "llm", "ai", "ml", "agent", "bot", "plugin",
        "skill", "memory", "memories", "note", "notes", "data", "database", "db",
        "store", "storage", "save", "saved", "read", "write", "written", "content",
        "text", "words", "word", "sentence", "paragraph", "page", "line", "token",
        "chunk", "section", "header", "title", "heading", "user", "users",
        "assistant", "message", "messages", "chat", "conversation", "turn", "turns",
        "prompt", "prompts", "response", "responses", "output", "outputs", "input",
        "inputs", "error", "errors", "warning", "warnings", "info", "detail",
        "details", "log", "logs", "debug", "bug", "bugs", "crash", "crashes",
        "failed", "failure", "success", "successful", "improve", "improved",
        "optimize", "optimized", "performance", "speed", "fast", "slow", "time",
        "second", "seconds", "minute", "minutes", "hour", "hours", "day", "days",
        "week", "weeks", "month", "months", "year", "years", "now", "today",
        "tomorrow", "yesterday", "recent", "recently", "current", "currently",
        "future", "past", "previous", "following", "however", "therefore", "thus",
        "hence", "otherwise", "meanwhile", "furthermore", "moreover", "besides",
        "either", "neither", "nor", "except", "including", "concerning", "regarding",
        "via", "per", "throughout", "across", "around", "near", "beside", "beyond",
        "beneath", "under", "overhead", "onto", "upon", "towards", "away", "off",
        "forward", "backward", "behind", "ahead", "ago", "yet", "always", "often",
        "frequently", "usually", "generally", "rarely", "seldom", "occasionally",
        "sometimes", "approximately", "precisely", "specifically", "mainly", "mostly",
        "largely", "chiefly", "primarily", "essentially", "fundamentally", "basically",
        "nearly", "almost", "roughly", "md", "txt", "rs", "py", "js", "ts", "html",
        "css", "json", "yaml", "yml", "toml", "cfg", "conf", "ini", "env", "git",
        "docker", "compose", "node", "modules", "package", "packages",
    ]
    .into_iter()
    .collect();

    let mut word_counts: HashMap<String, usize> = HashMap::new();

    for text in &texts {
        for word in text.split_whitespace() {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();

            if cleaned.len() >= 3 && !stopwords.contains(cleaned.as_str()) {
                *word_counts.entry(cleaned).or_insert(0) += 1;
            }
        }
    }

    let mut sorted: Vec<_> = word_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let top_words: Vec<String> = sorted
        .iter()
        .take(3)
        .filter(|(_, count)| *count >= 2) // Require at least 2 occurrences
        .map(|(w, _)| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect();

    if top_words.is_empty() {
        format!("Topic-{}", &uuid::Uuid::new_v4().to_string()[..6])
    } else {
        top_words.join(" ")
    }
}

/// Detect topic clusters from a set of memories using k-means clustering.
/// Returns a list of TopicClusters.
pub fn detect_topic_clusters(
    memories_with_vectors: &[(String, MemoryPoint, Vec<f32>)],
    max_clusters: usize,
) -> Vec<TopicCluster> {
    if memories_with_vectors.is_empty() {
        return vec![];
    }

    let n = memories_with_vectors.len();

    // Choose k: between 3 and max_clusters, but no more than n
    let k = max_clusters.min(n).max(1);

    let ids: Vec<String> = memories_with_vectors.iter().map(|(id, _, _)| id.clone()).collect();
    let vectors: Vec<Vec<f32>> = memories_with_vectors.iter().map(|(_, _, v)| v.clone()).collect();
    let memories: Vec<MemoryPoint> = memories_with_vectors.iter().map(|(_, m, _)| m.clone()).collect();

    let (centroids, assignments) = kmeans_topic_clusters(&vectors, &ids, k, 50);

    // Group memory IDs by cluster assignment
    let mut cluster_members: HashMap<i32, Vec<String>> = HashMap::new();
    for (i, assignment) in assignments.iter().enumerate() {
        cluster_members
            .entry(*assignment)
            .or_default()
            .push(ids[i].clone());
    }

    // Build TopicCluster structs
    let mut clusters: Vec<TopicCluster> = cluster_members
        .into_iter()
        .enumerate()
        .map(|(idx, (cid, memory_ids))| {
            let label = generate_cluster_label(&memory_ids, &memories);
            let total_strength: f32 = memory_ids.len() as f32 * 0.5; // Placeholder

            TopicCluster {
                id: format!("cluster-{}", cid),
                label,
                memory_ids,
                centroid: centroids.get(idx).cloned().unwrap_or_default(),
                total_link_strength: total_strength,
            }
        })
        .collect();

    // Sort by size descending
    clusters.sort_by(|a, b| b.memory_ids.len().cmp(&a.memory_ids.len()));

    // Re-number cluster IDs to be sequential
    for (i, cluster) in clusters.iter_mut().enumerate() {
        cluster.id = format!("cluster-{}", i);
    }

    tracing::info!("Detected {} topic clusters from {} memories", clusters.len(), n);

    clusters
}

/// Classify the relationship between two memories.
/// This is a heuristic classifier based on available metadata.
/// Returns (LinkType, confidence, explanation).
pub fn classify_relationship(
    memory_a: &MemoryPoint,
    memory_b: &MemoryPoint,
    similarity: f32,
) -> (LinkType, f32, String) {
    // If in same cluster or realm, likely companions
    if memory_a.realm_id == memory_b.realm_id && !memory_a.realm_id.is_none() {
        if similarity > 0.7 {
            return (
                LinkType::Companion,
                0.8,
                format!(
                    "Both memories are in realm '{}' with high similarity {:.2}",
                    memory_a.realm_name, similarity
                ),
            );
        }
    }

    // Check for temporal ordering from ingested_at timestamps
    if let (Ok(ts_a), Ok(ts_b)) = (
        chrono::DateTime::parse_from_rfc3339(&memory_a.ingested_at),
        chrono::DateTime::parse_from_rfc3339(&memory_b.ingested_at),
    ) {
        let diff = (ts_b - ts_a).num_seconds().abs();
        // If within 5 minutes of each other, likely part of same session
        if diff < 300 {
            return (
                LinkType::Companion,
                0.7,
                format!(
                    "Both memories ingested within {} seconds of each other",
                    diff
                ),
            );
        }
        // If one is shortly after the other
        if diff < 3600 && diff > 0 {
            return (
                LinkType::TemporalNext,
                0.6,
                format!(
                    "Memory B was ingested {} seconds after Memory A",
                    diff
                ),
            );
        }
    }

    // High similarity → companion
    if similarity > 0.75 {
        return (
            LinkType::Companion,
            similarity,
            format!("High semantic similarity {:.2}", similarity),
        );
    }

    // Medium similarity → might fill a gap
    if similarity > 0.5 && similarity <= 0.75 {
        return (
            LinkType::FillGap,
            similarity * 0.8,
            format!(
                "Moderate similarity {:.2} — B may fill a conceptual gap near A",
                similarity
            ),
        );
    }

    // Low similarity but same source file → might be prerequisite or companion
    if memory_a.source_file.is_some()
        && memory_a.source_file == memory_b.source_file
    {
        return (
            LinkType::Prerequisite,
            0.5,
            "Same source file but different topics — B may be a prerequisite for A"
                .to_string(),
        );
    }

    (LinkType::None, 0.3, "No clear relationship detected".to_string())
}

/// Build inferred associations for all memory pairs that are semantically close.
/// Returns a list of InferredLinks.
pub fn build_inferred_associations(
    memories: &[MemoryPoint],
    memory_vectors: &HashMap<String, Vec<f32>>,
    similarity_threshold: f32,
) -> Vec<InferredLink> {
    let mut links = Vec::new();

    // For each pair of memories, if they're similar enough, classify the relationship
    let n = memories.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let mem_a = &memories[i];
            let mem_b = &memories[j];

            let vec_a = match memory_vectors.get(&mem_a.id) {
                Some(v) => v,
                None => continue,
            };
            let vec_b = match memory_vectors.get(&mem_b.id) {
                Some(v) => v,
                None => continue,
            };

            let similarity = cosine_similarity(vec_a, vec_b);

            if similarity < similarity_threshold {
                continue;
            }

            let (link_type, confidence, explanation) =
                classify_relationship(mem_a, mem_b, similarity);

            if link_type != LinkType::None {
                links.push(InferredLink {
                    from_id: mem_a.id.clone(),
                    to_id: mem_b.id.clone(),
                    link_type: link_type.clone(),
                    confidence,
                    explanation: explanation.clone(),
                });

                // Add reverse link for bidirectional associations
                if link_type == LinkType::Companion || link_type == LinkType::SameCluster {
                    links.push(InferredLink {
                        from_id: mem_b.id.clone(),
                        to_id: mem_a.id.clone(),
                        link_type: link_type.clone(),
                        confidence,
                        explanation: explanation.clone(),
                    });
                }
            }
        }
    }

    tracing::debug!(
        "Built {} inferred associations from {} memories (threshold={:.2})",
        links.len(),
        n,
        similarity_threshold
    );

    links
}

/// Detect knowledge gaps: areas where there are related topics but no memories bridging them.
/// Returns a list of Gaps.
pub fn detect_gaps(
    clusters: &[TopicCluster],
    memories: &[MemoryPoint],
    memory_vectors: &HashMap<String, Vec<f32>>,
) -> Vec<Gap> {
    let mut gaps = Vec::new();

    if clusters.len() < 2 {
        return gaps;
    }

    // For each pair of clusters, check if there's a bridge memory
    for i in 0..clusters.len() {
        for j in (i + 1)..clusters.len() {
            let cluster_a = &clusters[i];
            let cluster_b = &clusters[j];

            // Find the memories closest to the boundary between clusters
            // (memories in A that are most similar to centroids of B)
            let mut best_bridge_a: Option<(&MemoryPoint, f32)> = None;
            let mut best_bridge_b: Option<(&MemoryPoint, f32)> = None;

            for mem_id in &cluster_a.memory_ids {
                if let Some(vec) = memory_vectors.get(mem_id) {
                    let sim_b = cosine_similarity(vec, &cluster_b.centroid);
                    if let Some((_, prev)) = best_bridge_a {
                        if sim_b > prev {
                            let mem = memories.iter().find(|m| m.id == *mem_id);
                            if let Some(m) = mem {
                                best_bridge_a = Some((m, sim_b));
                            }
                        }
                    } else {
                        let mem = memories.iter().find(|m| m.id == *mem_id);
                        if let Some(m) = mem {
                            best_bridge_a = Some((m, sim_b));
                        }
                    }
                }
            }

            for mem_id in &cluster_b.memory_ids {
                if let Some(vec) = memory_vectors.get(mem_id) {
                    let sim_a = cosine_similarity(vec, &cluster_a.centroid);
                    if let Some((_, prev)) = best_bridge_b {
                        if sim_a > prev {
                            let mem = memories.iter().find(|m| m.id == *mem_id);
                            if let Some(m) = mem {
                                best_bridge_b = Some((m, sim_a));
                            }
                        }
                    } else {
                        let mem = memories.iter().find(|m| m.id == *mem_id);
                        if let Some(m) = mem {
                            best_bridge_b = Some((m, sim_a));
                        }
                    }
                }
            }

            // If both clusters have memories but they're far apart, there's a gap
            let (Some((bridge_a, sim_a_to_b)), Some((bridge_b, sim_b_to_a))) =
                (best_bridge_a, best_bridge_b)
            else {
                continue;
            };

            // The gap exists if neither memory is very close to the other cluster
            if sim_a_to_b < 0.4 && sim_b_to_a < 0.4 {
                // These clusters are semantically distant but topically related (based on
                // k-means being confident they're separate clusters — i.e., they are
                // distinct but might benefit from a bridging memory).
                let gap = Gap::new(
                    cluster_a.label.clone(),
                    cluster_b.label.clone(),
                    LinkType::FillGap,
                    format!(
                        "Topic cluster '{}' and '{}' are related but lack a bridging memory. \
                         Consider adding content that connects {} to {}.",
                        cluster_a.label,
                        cluster_b.label,
                        cluster_a.label,
                        cluster_b.label
                    ),
                    0.7,
                );
                gaps.push(gap);
            }

            // Check for temporal gaps: if cluster A memories are all older than B, but
            // there's no transitional memory, suggest a temporal gap
            let all_a_older = cluster_a.memory_ids.iter().all(|id| {
                memories
                    .iter()
                    .find(|m| m.id == *id)
                    .map(|m| {
                        chrono::DateTime::parse_from_rfc3339(&m.ingested_at)
                            .map(|dt| dt.timestamp())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0)
                    < cluster_b
                        .memory_ids
                        .iter()
                        .filter_map(|id| memories.iter().find(|m| m.id == *id))
                        .filter_map(|m| {
                            chrono::DateTime::parse_from_rfc3339(&m.ingested_at)
                                .map(|dt| dt.timestamp())
                                .ok()
                        })
                        .max()
                        .unwrap_or(0)
            });

            if all_a_older && sim_a_to_b > 0.3 && sim_a_to_b < 0.6 {
                let gap = Gap::new(
                    cluster_a.label.clone(),
                    cluster_b.label.clone(),
                    LinkType::TemporalNext,
                    format!(
                        "'{}' appears to precede '{}' chronologically but there's no \
                         clear transitional memory connecting them.",
                        cluster_a.label, cluster_b.label
                    ),
                    0.6,
                );
                gaps.push(gap);
            }
        }
    }

    tracing::info!(
        "Detected {} knowledge gaps across {} clusters",
        gaps.len(),
        clusters.len()
    );

    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 0.001);

        let d = vec![0.707, 0.707, 0.0];
        let sim = cosine_similarity(&a, &d);
        assert!((sim - 0.707).abs() < 0.01);
    }

    #[test]
    fn test_kmeans_simple() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.1],
            vec![10.0, 10.0],
            vec![10.1, 10.1],
        ];
        let ids = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let (centroids, assignments) = kmeans_topic_clusters(&vectors, &ids, 2, 20);

        assert_eq!(centroids.len(), 2);
        // First two should be in one cluster, last two in another
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[2], assignments[3]);
        assert_ne!(assignments[0], assignments[2]);
    }

    #[test]
    fn test_classify_relationship() {
        let mem_a = MemoryPoint {
            id: "a".into(),
            content: "Rust is a systems programming language".into(),
            summary: None,
            source_file: Some("rust.md".into()),
            realm_id: Some("r1".into()),
            realm_name: "Programming".into(),
            importance: 0.8,
            upvotes: 1,
            tags: vec![],
            ingested_at: "2024-01-01T00:00:00Z".into(),
            last_accessed: "2024-01-01T00:00:00Z".into(),
            access_count: 5,
            chunk_type: "section".into(),
            heading: Some("Rust intro".into()),
            source_hash: "abc".into(),
            related_memory_ids: vec![],
            association_strengths: vec![],
            reaction_score: 0.0,
            topic_clusters: vec![],
        };

        let mem_b = MemoryPoint {
            id: "b".into(),
            content: "Rust ownership and borrowing explained".into(),
            summary: None,
            source_file: Some("rust.md".into()),
            realm_id: Some("r1".into()),
            realm_name: "Programming".into(),
            importance: 0.8,
            upvotes: 1,
            tags: vec![],
            ingested_at: "2024-01-01T00:05:00Z".into(),
            last_accessed: "2024-01-01T00:00:00Z".into(),
            access_count: 5,
            chunk_type: "section".into(),
            heading: Some("Rust ownership".into()),
            source_hash: "def".into(),
            related_memory_ids: vec![],
            association_strengths: vec![],
            reaction_score: 0.0,
            topic_clusters: vec![],
        };

        let (link_type, confidence, _) = classify_relationship(&mem_a, &mem_b, 0.8);
        assert_eq!(link_type, LinkType::Companion);
        assert!(confidence > 0.5);
    }
}
