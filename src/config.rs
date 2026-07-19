use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub embedding: EmbeddingConfig,
    pub qdrant: QdrantConfig,
    pub ingest: IngestConfig,
    pub realms: RealmsConfig,
    pub slumber: SlumberConfig,
    pub memex8_md: Memex8MdConfig,
    pub digest_md: DigestMdConfig,
    pub web: WebConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub watch: Vec<WatchConfig>,
    /// TurboVec configuration for vector compression.
    #[serde(default)]
    pub turbovec: TurbovecConfig,
    /// Memory verification sweep configuration (slumber Phase 13).
    #[serde(default)]
    pub verification: VerificationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub mcp_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub model: String,
    pub dimensions: u32,
    #[serde(default)]
    pub ollama: OllamaConfig,
    #[serde(default)]
    pub openai: OpenAiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    #[serde(default = "default_ollama_url")]
    pub url: String,
    /// Maximum number of concurrent embedding requests (default: 8).
    #[serde(default = "default_ollama_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}

fn default_ollama_max_concurrent() -> usize {
    8
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            url: default_ollama_url(),
            max_concurrent: default_ollama_max_concurrent(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiConfig {
    #[serde(default = "default_openai_key_env")]
    pub api_key_env: String,
    /// Base URL for OpenAI-compatible APIs (e.g. MiniMax, Together, Groq).
    /// Defaults to `https://api.openai.com/v1`.
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,
    #[serde(default = "default_openai_model")]
    pub model: String,
    #[serde(default = "default_openai_dims")]
    pub dimensions: u32,
}

fn default_openai_key_env() -> String {
    "OPENAI_API_KEY".into()
}
fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".into()
}
fn default_openai_model() -> String {
    "text-embedding-3-small".into()
}
fn default_openai_dims() -> u32 {
    1536
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_openai_key_env(),
            base_url: default_openai_base_url(),
            model: default_openai_model(),
            dimensions: default_openai_dims(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    #[serde(default = "default_qdrant_url")]
    pub url: String,
    #[serde(default = "default_memories")]
    pub collection_memories: String,
    // collection_quantized removed — TurboVec handles vector compression
    #[serde(default = "default_realms")]
    pub collection_realms: String,
    #[serde(default = "default_graph_edges")]
    pub collection_graph_edges: String,
}

fn default_qdrant_url() -> String {
    "http://localhost:6333".into()
}
fn default_memories() -> String {
    "memories".into()
}
// collection_quantized removed — TurboVec handles vector compression
fn default_realms() -> String {
    "realms".into()
}
fn default_graph_edges() -> String {
    "graph_edges".into()
}


impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            collection_memories: default_memories(),
            // collection_quantized removed — TurboVec handles vector compression
            collection_realms: default_realms(),
            collection_graph_edges: default_graph_edges(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestConfig {
    pub default_chunk_by: String,
    pub max_chunk_tokens: u32,
    pub poll_interval: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmsConfig {
    pub auto_discover: bool,
    pub similarity_threshold: f32,
    pub split_threshold: u32,
    pub merge_threshold: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlumberConfig {
    pub idle_timeout: String,
    pub cron_ingest: String,
    pub quantize_bit_width: f32,
    pub auto_archive_days: u32,
    pub prune_threshold: f32,
    /// How much to bump importance each time a memory is recalled (touched).
    #[serde(default = "default_touch_importance_bump")]
    pub touch_importance_bump: f32,
    #[serde(default)]
    pub summarize: SummarizeConfig,
    /// Cron schedule for LLM consolidation (Phase 6).
    /// Default: "0 3 * * *" (daily at 3am).
    /// Set to "" to disable schedule-based consolidation.
    #[serde(default = "default_consolidation_schedule")]
    pub consolidation_schedule: String,
    /// Consolidation backend config.
    #[serde(default)]
    pub consolidation: ConsolidationConfig,
    /// Daily decay rate for memory importance (forgetting curve).
    #[serde(default = "default_decay_rate_per_day")]
    pub decay_rate_per_day: f32,
    /// Number of nearest neighbors to link per memory during association phase.
    #[serde(default = "default_association_top_k")]
    pub association_top_k: u32,
    /// Minimum cosine similarity to create an association link.
    #[serde(default = "default_association_min_strength")]
    pub association_min_strength: f32,
    /// Importance bump for associated memories during spreading activation.
    #[serde(default = "default_spreading_activation_bump")]
    pub spreading_activation_bump: f32,
    /// Number of topic clusters to detect (k for k-means).
    #[serde(default = "default_topic_clusters_k")]
    pub topic_clusters_k: u32,
    /// Similarity threshold for inferring associations.
    #[serde(default = "default_inference_similarity_threshold")]
    pub inference_similarity_threshold: f32,
}

fn default_topic_clusters_k() -> u32 {
    8
}
fn default_inference_similarity_threshold() -> f32 {
    0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceConfig {
    /// Number of topic clusters to detect (k for k-means).
    #[serde(default = "default_topic_clusters_k")]
    pub topic_clusters_k: u32,
    /// Minimum cosine similarity to create an inferred link.
    #[serde(default = "default_inference_similarity_threshold")]
    pub inference_similarity_threshold: f32,
    /// Whether to enable proactive gap detection.
    #[serde(default = "default_gap_detection_enabled")]
    pub gap_detection_enabled: bool,
}

fn default_gap_detection_enabled() -> bool {
    true
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            topic_clusters_k: 8,
            inference_similarity_threshold: 0.5,
            gap_detection_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeConfig {
    pub enabled: bool,
    pub max_cluster_size: u32,
    pub preserve_originals: bool,
    pub confidence_threshold: f32,
}

impl Default for SummarizeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_cluster_size: 20,
            preserve_originals: true,
            confidence_threshold: 0.8,
        }
    }
}

/// Consolidation backend configuration.
/// Supports both OpenAI (default, cheap for this use case) and local LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    /// Backend to use: "openai" (default) or "local".
    #[serde(default = "default_consolidation_backend")]
    pub backend: String,
    /// Model to use for consolidation.
    /// OpenAI: "gpt-4o-mini" (default) or "gpt-4o".
    /// Local: model name passed to the local LLM endpoint.
    #[serde(default)]
    pub model: Option<String>,
}

fn default_consolidation_backend() -> String {
    "openai".into()
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            backend: "openai".into(),
            model: Some("gpt-4o-mini".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexOptimizationConfig {
    pub enabled: bool,
    pub deleted_threshold: f32,
    pub vacuum_min_vector_number: u32,
    pub default_segment_number: u32,
    pub max_segment_size: u64,
    pub memmap_threshold: u64,
    pub indexing_threshold: u64,
    pub flush_interval_sec: u64,
    pub max_optimization_threads: u32,
}

impl Default for IndexOptimizationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            deleted_threshold: 0.1,
            vacuum_min_vector_number: 1000,
            default_segment_number: 4,
            max_segment_size: 200000,
            memmap_threshold: 50000,
            indexing_threshold: 20000,
            flush_interval_sec: 5,
            max_optimization_threads: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memex8MdConfig {
    pub enabled: bool,
    pub max_memories: u32,
    pub update_on_slumber: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestMdConfig {
    pub enabled: bool,
    pub path: String,
    pub max_memories: u32,
    pub include_realms: bool,
    pub max_log_entries: u32,
}

impl Default for DigestMdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "~/.memex8/memex8.md".into(),
            max_memories: 20,
            include_realms: true,
            max_log_entries: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    pub enabled: bool,
    pub theme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    pub path: String,
    #[serde(default = "default_chunk")]
    pub chunk_by: String,
    #[serde(default = "default_poll")]
    pub poll_interval: String,
    #[serde(default)]
    pub realm_hint: Option<String>,
}

fn default_chunk() -> String {
    "section".into()
}
fn default_poll() -> String {
    "5m".into()
}
fn default_touch_importance_bump() -> f32 {
    0.02
}
fn default_consolidation_schedule() -> String {
    "0 3 * * *".into()
}
fn default_decay_rate_per_day() -> f32 {
    0.001
}
fn default_association_top_k() -> u32 {
    5
}
fn default_association_min_strength() -> f32 {
    0.6
}
fn default_spreading_activation_bump() -> f32 {
    0.005
}

/// TurboVec configuration for vector compression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurbovecConfig {
    /// Bit width for TurboQuant compression (2 or 4).
    #[serde(default = "default_turbovec_bit_width")]
    pub bit_width: usize,
    /// Path to persist the TurboVec index.
    #[serde(default = "default_turbovec_index_path")]
    pub index_path: String,
    /// Companion file for id_map (turbovec doesn't persist custom IDs).
    #[serde(default = "default_turbovec_id_map_path")]
    pub id_map_path: String,
}

fn default_turbovec_bit_width() -> usize {
    4
}
fn default_turbovec_index_path() -> String {
    std::env::var("TURBOVEC_INDEX_PATH")
        .unwrap_or_else(|_| "data/memories.tv".into())
}
fn default_turbovec_id_map_path() -> String {
    std::env::var("TURBOVEC_ID_MAP_PATH")
        .unwrap_or_else(|_| "data/memories_ids.json".into())
}

impl Default for TurbovecConfig {
    fn default() -> Self {
        Self {
            bit_width: default_turbovec_bit_width(),
            index_path: default_turbovec_index_path(),
            id_map_path: default_turbovec_id_map_path(),
        }
    }
}

/// Memory verification sweep configuration (slumber Phase 13).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
    /// Whether the verification sweep runs during slumber.
    #[serde(default = "default_verification_enabled")]
    pub enabled: bool,
    /// Approximate number of memories sampled per sweep.
    #[serde(default = "default_verification_sample_size")]
    pub sample_size: usize,
    /// Don't re-verify anything verified fewer than this many days ago.
    #[serde(default = "default_verification_min_interval_days")]
    pub min_interval_days: u64,
    /// Confidence >= this maps to "verified".
    #[serde(default = "default_verification_stale_threshold")]
    pub stale_threshold: f32,
    /// Confidence >= this (but below stale_threshold) maps to "stale"; below maps to "contradicted".
    #[serde(default = "default_verification_contradicted_threshold")]
    pub contradicted_threshold: f32,
}

fn default_verification_enabled() -> bool {
    true
}
fn default_verification_sample_size() -> usize {
    30
}
fn default_verification_min_interval_days() -> u64 {
    7
}
fn default_verification_stale_threshold() -> f32 {
    0.8
}
fn default_verification_contradicted_threshold() -> f32 {
    0.4
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            enabled: default_verification_enabled(),
            sample_size: default_verification_sample_size(),
            min_interval_days: default_verification_min_interval_days(),
            stale_threshold: default_verification_stale_threshold(),
            contradicted_threshold: default_verification_contradicted_threshold(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let config_path = Path::new(path);
        if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            let config: AppConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            tracing::warn!("Config file not found at {}, using defaults", path);
            Ok(Self::default())
        }
    }

    /// Get the API key from environment
    pub fn api_key(&self) -> Option<String> {
        std::env::var(&self.auth.api_key_env).ok()
    }

    /// Get the OpenAI API key from environment
    pub fn openai_api_key(&self) -> Option<String> {
        std::env::var(&self.embedding.openai.api_key_env).ok()
    }

    /// Get the active embedding dimensions
    pub fn embedding_dimensions(&self) -> u32 {
        self.embedding.dimensions
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                mcp_port: 8081,
            },
            auth: AuthConfig {
                api_key_env: "MEMEX8_API_KEY".into(),
            },
            embedding: EmbeddingConfig {
                provider: "ollama".into(),
                model: "nomic-embed-text".into(),
                dimensions: 768,
                ollama: OllamaConfig::default(),
                openai: OpenAiConfig::default(),
            },
            qdrant: QdrantConfig::default(),
            ingest: IngestConfig {
                default_chunk_by: "section".into(),
                max_chunk_tokens: 2000,
                poll_interval: "5m".into(),
            },
            realms: RealmsConfig {
                auto_discover: true,
                similarity_threshold: 0.75,
                split_threshold: 100,
                merge_threshold: 0.3,
            },
            slumber: SlumberConfig {
                idle_timeout: "10m".into(),
                cron_ingest: "*/5 * * * *".into(),
                quantize_bit_width: 3.5,
                auto_archive_days: 90,
                prune_threshold: 0.1,
                touch_importance_bump: 0.02,
                summarize: SummarizeConfig::default(),
                consolidation_schedule: "0 3 * * *".into(),
                consolidation: ConsolidationConfig::default(),
                decay_rate_per_day: 0.001,
                association_top_k: 5,
                association_min_strength: 0.6,
                spreading_activation_bump: 0.005,
                topic_clusters_k: 8,
                inference_similarity_threshold: 0.5,
            },
            memex8_md: Memex8MdConfig {
                enabled: true,
                max_memories: 20,
                update_on_slumber: true,
            },
            digest_md: DigestMdConfig::default(),
            web: WebConfig {
                enabled: true,
                theme: "dark".into(),
            },
            inference: InferenceConfig::default(),
            watch: vec![],
            turbovec: TurbovecConfig::default(),
            verification: VerificationConfig::default(),
        }
    }
}
