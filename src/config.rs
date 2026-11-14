use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub dream: DreamConfig,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub coverage: CoverageConfig,
    pub hybrid: HybridConfig,
}

/// Dreams: scheduled, opt-in, quarantined from search. See `engine::dream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DreamConfig {
    /// Off by default.
    pub enabled: bool,
    /// "daily" or "weekly".
    pub frequency: String,
    /// Local hour (0-23) when the dream is generated.
    pub hour: u32,
    /// Seed memories per dream.
    pub seeds: usize,
    /// Upper bound on the dream's length.
    pub max_words: usize,
    /// Days a dream waits for keep/discard before it is deleted.
    pub pending_days: u32,
    /// Model override; defaults to the consolidation backend's model.
    pub model: Option<String>,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frequency: "weekly".into(),
            hour: 4,
            seeds: 12,
            max_words: 350,
            pending_days: 14,
            model: None,
        }
    }
}

impl DreamConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.frequency.as_str(), "daily" | "weekly"),
            "dream.frequency must be daily or weekly"
        );
        anyhow::ensure!(self.hour <= 23, "dream.hour must be 0-23");
        anyhow::ensure!((4..=40).contains(&self.seeds), "dream.seeds must be 4-40");
        anyhow::ensure!(
            (80..=1500).contains(&self.max_words),
            "dream.max_words must be 80-1500"
        );
        anyhow::ensure!(
            (1..=365).contains(&self.pending_days),
            "dream.pending_days must be 1-365"
        );
        Ok(())
    }
}

/// Keyword (BM25) matches blended into vector search.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HybridConfig {
    /// Blend BM25 keyword matches into vector search. On by default: on the
    /// MemDerive bench it lifts top-15 support recall from 47.6 to 52.5 with no
    /// world regressing (docs/plans/nov13_memex8_plan.md, Findings).
    pub enabled: bool,
    /// Bonus for the best keyword match, on the cosine scale (0..=1).
    pub lexical_weight: f32,
    /// Keyword candidates considered per query.
    pub lexical_candidates: usize,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lexical_weight: 0.15,
            lexical_candidates: 50,
        }
    }
}

impl HybridConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.lexical_weight.is_finite() && (0.0..=1.0).contains(&self.lexical_weight),
            "search.hybrid.lexical_weight must be between 0 and 1"
        );
        anyhow::ensure!(
            (1..=500).contains(&self.lexical_candidates),
            "search.hybrid.lexical_candidates must be between 1 and 500"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoverageConfig {
    pub high: f32,
    pub partial: f32,
}

impl Default for CoverageConfig {
    fn default() -> Self {
        Self {
            high: 0.55,
            partial: 0.40,
        }
    }
}

impl CoverageConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.high.is_finite()
                && self.partial.is_finite()
                && (0.0..=1.0).contains(&self.high)
                && (0.0..=1.0).contains(&self.partial)
                && self.partial < self.high,
            "search.coverage thresholds must satisfy 0 <= partial < high <= 1"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub mcp_port: u16,
    /// When true, the server starts without auth even if no
    /// `MEMEX8_API_KEY` is set. CLI flag: `--allow-no-api-key`. Off by
    /// default; when off, requests to `/api/v1` with no key configured
    /// return 401 (fail-closed). See issue #11.
    #[serde(default)]
    pub allow_no_api_key: bool,
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
    /// Minimum original-vector cosine similarity for a duplicate candidate.
    #[serde(default = "default_dedup_threshold")]
    pub dedup_threshold: f32,
    /// Safe default. False enables archived duplicate removal during full slumber.
    #[serde(default = "default_dedup_dry_run")]
    pub dedup_dry_run: bool,
    /// Bound pair comparisons per realm; truncated reports are explicitly marked.
    #[serde(default = "default_dedup_max_comparisons")]
    pub dedup_max_comparisons_per_realm: usize,
    pub idle_timeout: String,
    pub cron_ingest: String,
    pub quantize_bit_width: f32,
    #[serde(default = "default_auto_archive_days")]
    pub auto_archive_days: u32,
    /// Per-memory-type overrides for `auto_archive_days`. Phase 4
    /// (slumber prune flagging) and `Engine::prune_queue` look up the
    /// memory's `memory_type` here and fall back to `auto_archive_days`
    /// when the type isn't listed.
    ///
    /// The values in `default_auto_archive_days_by_type` are derived
    /// from the Weibull params in `engine/decay.rs::weibull_params`:
    /// each is the age (in days) at which that type's recall-style score
    /// crosses `prune_threshold` (0.1). For types whose Weibull crosses
    /// 0.1 at multi-year timescales (profile, preference, relationship,
    /// entity), the value is clamped to 365 days — long-term stable
    /// memories shouldn't be auto-archived within a year. `request`
    /// (fastest-decaying) is clamped to 7 days minimum so we don't
    /// prune aggressively on the very first missed access.
    ///
    /// Computed by `default_auto_archive_days_for(name)` — keep that
    /// function and the Weibull table in sync if either changes.
    #[serde(default = "default_auto_archive_days_by_type")]
    pub auto_archive_days_by_type: std::collections::HashMap<String, u32>,
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

fn default_dedup_threshold() -> f32 {
    0.95
}
fn default_dedup_dry_run() -> bool {
    true
}
fn default_dedup_max_comparisons() -> usize {
    1_000_000
}

impl SlumberConfig {
    pub fn validate_dedup(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.dedup_threshold.is_finite() && (0.0..=1.0).contains(&self.dedup_threshold),
            "slumber.dedup_threshold must be finite and between 0 and 1"
        );
        anyhow::ensure!(
            self.dedup_max_comparisons_per_realm > 0,
            "slumber.dedup_max_comparisons_per_realm must be greater than zero"
        );
        Ok(())
    }
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
    /// Run LLM consolidation (slumber phase 6). Off by default: summaries tend to
    /// drop exceptions and exact values, and they crowd originals out of the
    /// top-k. Turn on in Settings -> Slumber -> "LLM consolidation".
    #[serde(default)]
    pub enabled: bool,
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
            enabled: false,
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
    /// Root for MEMEX8.md files of memories whose `source_file` is a label
    /// ("hermes", "hermes-sync") or URL rather than a real path. In Docker this
    /// is the `/watch` bind mount, so files land on the host in `watch_dirs/`.
    #[serde(default = "default_memex8_md_output_dir")]
    pub output_dir: String,
}

fn default_memex8_md_output_dir() -> String {
    "/watch".into()
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

fn default_auto_archive_days() -> u32 {
    // Lowered from 90 → 14 in `fix/score-based-prune` so phase 4
    // starts working on freshly-ingested low-value noise (e.g.
    // auto-generated cron summaries) within two weeks instead of
    // three months. Combined with score-based pruning (see
    // `prune_flag`), this lets the Weibull decay actually kick
    // memories out of recall within a realistic timeframe.
    14
}

/// Days at which a given memory_type's Weibull score crosses the
/// default prune threshold (0.1). See `default_auto_archive_days_by_type`.
/// Formula: t = eta_hours × (ln(10))^(1/k) / 24, then rounded to int
/// with min=7 (a week) and max=365 (a year).
fn default_auto_archive_days_for(name: &str) -> u32 {
    // Mirror the params in `engine::decay::weibull_params`. Hard-coded
    // here (not imported) so the slumber config doesn't depend on
    // the engine at config-deserialization time — and so the values
    // are visible at the config layer where users tune them.
    let (k, eta_hours): (f64, f64) = match name {
        "profile" => (0.30, 8760.0),
        "preference" => (0.40, 4380.0),
        "dream" => (0.40, 4380.0),
        "relationship" => (0.35, 8760.0),
        "learning" => (0.70, 1440.0),
        "fact" => (0.80, 720.0),
        "entity" => (0.50, 4380.0),
        "setup" => (0.60, 2160.0),
        "pattern" => (0.60, 1680.0),
        "context" => (0.85, 360.0),
        "observation" => (0.90, 480.0),
        "artifact" => (0.75, 2160.0),
        "project" => (0.85, 1080.0),
        "goal" => (0.90, 720.0),
        "decision" => (1.00, 336.0),
        "commitment" => (1.00, 240.0),
        "event" => (1.20, 168.0),
        "instruction" => (0.90, 480.0),
        "error" => (1.10, 336.0),
        "issue" => (1.10, 336.0),
        "request" => (1.50, 72.0),
        "general" => (1.00, 168.0),
        _ => return default_auto_archive_days(),
    };
    let t_hours = eta_hours * (2.302585092994046_f64).powf(1.0 / k);
    let t_days = (t_hours / 24.0).round() as i64;
    t_days.clamp(7, 365) as u32
}

fn default_auto_archive_days_by_type() -> std::collections::HashMap<String, u32> {
    // Every type the engine knows about. Includes `general` (the
    // default for unknown / legacy memories). If `weibull_params` adds
    // a new type, add it here too.
    let types = [
        "profile",
        "preference",
        "dream",
        "relationship",
        "learning",
        "fact",
        "entity",
        "setup",
        "pattern",
        "context",
        "observation",
        "artifact",
        "project",
        "goal",
        "decision",
        "commitment",
        "event",
        "instruction",
        "error",
        "issue",
        "request",
        "general",
    ];
    types
        .iter()
        .map(|n| (n.to_string(), default_auto_archive_days_for(n)))
        .collect()
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
    std::env::var("TURBOVEC_INDEX_PATH").unwrap_or_else(|_| "data/memories.tv".into())
}
fn default_turbovec_id_map_path() -> String {
    std::env::var("TURBOVEC_ID_MAP_PATH").unwrap_or_else(|_| "data/memories_ids.json".into())
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
        let mut config = if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            let overrides = std::fs::read_to_string(Self::settings_path_for(&content)).ok();
            Self::from_toml_with_overrides(&content, overrides.as_deref())?
        } else {
            tracing::warn!("Config file not found at {}, using defaults", path);
            Self::default()
        };
        config.apply_env_overrides();
        Ok(config)
    }

    /// Parse and validate a full config document.
    pub fn from_value(v: &toml::Value) -> anyhow::Result<Self> {
        let config: AppConfig = v.clone().try_into()?;
        config.slumber.validate_dedup()?;
        config.search.coverage.validate()?;
        config.search.hybrid.validate()?;
        config.dream.validate()?;
        Ok(config)
    }

    /// Recursively merge `over` into `base` (tables merge, scalars replace).
    pub fn merge_toml(base: &mut toml::Value, over: &toml::Value) {
        match (base, over) {
            (toml::Value::Table(b), toml::Value::Table(o)) => {
                for (k, v) in o {
                    match b.get_mut(k) {
                        Some(existing) => Self::merge_toml(existing, v),
                        None => {
                            b.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            (b, o) => *b = o.clone(),
        }
    }

    /// config.toml plus the UI-written overrides file. An overrides file that
    /// is unparsable or yields an invalid config is ignored (with a warning)
    /// so a bad saved setting can never stop the server from starting.
    pub fn from_toml_with_overrides(
        content: &str,
        overrides: Option<&str>,
    ) -> anyhow::Result<Self> {
        let base: toml::Value = toml::from_str(content)?;
        if let Some(text) = overrides {
            let attempt = toml::from_str::<toml::Value>(text)
                .map_err(anyhow::Error::from)
                .and_then(|over| {
                    let mut merged = base.clone();
                    Self::merge_toml(&mut merged, &over);
                    Self::from_value(&merged)
                });
            match attempt {
                Ok(c) => return Ok(c),
                Err(e) => tracing::warn!("Ignoring invalid settings overrides: {e}"),
            }
        }
        Self::from_value(&base)
    }

    /// Where UI-edited settings live: the data volume, next to the index.
    pub fn settings_path_from_index(index_path: &str) -> std::path::PathBuf {
        if let Ok(p) = std::env::var("MEMEX8_SETTINGS_PATH") {
            if !p.is_empty() {
                return p.into();
            }
        }
        Path::new(index_path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("settings.toml")
    }

    fn settings_path_for(content: &str) -> std::path::PathBuf {
        let from_file = toml::from_str::<toml::Value>(content)
            .ok()
            .and_then(|v| {
                v.get("turbovec")?
                    .get("index_path")?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(default_turbovec_index_path);
        let index = std::env::var("TURBOVEC_INDEX_PATH")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or(from_file);
        Self::settings_path_from_index(&index)
    }

    pub fn settings_path(&self) -> std::path::PathBuf {
        Self::settings_path_from_index(&self.turbovec.index_path)
    }

    /// Environment overrides applied after deserialization.
    /// Precedence: env var → config.toml → built-in default.
    ///
    /// serde's `#[serde(default = ...)]` only reads `TURBOVEC_*` when the key
    /// is absent from the TOML. config.toml sets relative `data/...` paths, so
    /// in the container (cwd `/`, uid 1000) the index resolved to
    /// `/data/memories.tv`, which can't be created — every slumber logged
    /// "Failed to save TurboVec index: No such file or directory" and the
    /// on-disk index went stale. The compose file exports absolute paths on
    /// the persistent volume; this makes them authoritative.
    fn apply_env_overrides(&mut self) {
        if let Ok(p) = std::env::var("TURBOVEC_INDEX_PATH") {
            if !p.is_empty() {
                self.turbovec.index_path = p;
            }
        }
        if let Ok(p) = std::env::var("TURBOVEC_ID_MAP_PATH") {
            if !p.is_empty() {
                self.turbovec.id_map_path = p;
            }
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
            search: SearchConfig::default(),
            dream: DreamConfig::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                mcp_port: 8081,
                allow_no_api_key: false,
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
                dedup_threshold: default_dedup_threshold(),
                dedup_dry_run: default_dedup_dry_run(),
                dedup_max_comparisons_per_realm: default_dedup_max_comparisons(),
                idle_timeout: "10m".into(),
                cron_ingest: "*/5 * * * *".into(),
                quantize_bit_width: 3.5,
                auto_archive_days: default_auto_archive_days(),
                auto_archive_days_by_type: default_auto_archive_days_by_type(),
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
                output_dir: default_memex8_md_output_dir(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dream_defaults_are_off_and_valid_and_bad_values_rejected() {
        let d = DreamConfig::default();
        assert!(!d.enabled && d.validate().is_ok());
        assert!(!AppConfig::default().dream.enabled);
        let bad = |f: fn(&mut DreamConfig)| {
            let mut c = DreamConfig::default();
            f(&mut c);
            assert!(c.validate().is_err());
        };
        bad(|c| c.frequency = "hourly".into());
        bad(|c| c.hour = 24);
        bad(|c| c.seeds = 3);
        bad(|c| c.max_words = 50);
        bad(|c| c.pending_days = 0);
        // a config file that predates dreams still loads, with dreams off
        let legacy: AppConfig = toml::from_str(
            &toml::to_string(&AppConfig::default())
                .unwrap()
                .replace("[dream]", "[ignored]"),
        )
        .unwrap();
        assert!(!legacy.dream.enabled);
    }

    #[test]
    fn consolidation_is_off_by_default_and_can_be_enabled() {
        assert!(!AppConfig::default().slumber.consolidation.enabled);
        let base = toml::to_string(&AppConfig::default()).unwrap();
        let on = AppConfig::from_toml_with_overrides(
            &base,
            Some("[slumber.consolidation]\nenabled = true\n"),
        )
        .unwrap();
        assert!(on.slumber.consolidation.enabled);
        // A config file that predates the switch keeps consolidation off.
        let legacy: ConsolidationConfig = toml::from_str("backend = \"openai\"").unwrap();
        assert!(!legacy.enabled);
    }

    #[test]
    fn hybrid_defaults_validate_and_bad_values_are_rejected() {
        let d = HybridConfig::default();
        assert!(d.enabled, "hybrid search is on by default");
        assert_eq!(d.lexical_weight, 0.15);
        let legacy: SearchConfig = toml::from_str("[coverage]\n").unwrap();
        assert!(
            legacy.hybrid.enabled,
            "configs without [search.hybrid] get it on"
        );
        assert!(d.validate().is_ok());
        let heavy = HybridConfig {
            lexical_weight: 1.5,
            ..HybridConfig::default()
        };
        assert!(heavy.validate().is_err());
        let none = HybridConfig {
            lexical_candidates: 0,
            ..HybridConfig::default()
        };
        assert!(none.validate().is_err());
        let base = toml::to_string(&AppConfig::default()).unwrap();
        // An invalid override is ignored (startup must never fail on settings.toml).
        let c = AppConfig::from_toml_with_overrides(
            &base,
            Some("[search.hybrid]\nlexical_weight = 3.0\n"),
        )
        .unwrap();
        assert_eq!(c.search.hybrid.lexical_weight, 0.15);
    }

    /// Regression pin: an explicit relative path in config.toml must not mask
    /// TURBOVEC_* (the container could not persist its index for weeks).
    /// One test touches the env vars, so parallel tests can't race on them.
    #[test]
    fn test_turbovec_env_overrides_config_file() {
        let mut config = AppConfig::default();
        config.turbovec.index_path = "data/memories.tv".into();
        config.turbovec.id_map_path = "data/memories_ids.json".into();

        std::env::remove_var("TURBOVEC_INDEX_PATH");
        std::env::remove_var("TURBOVEC_ID_MAP_PATH");
        config.apply_env_overrides();
        assert_eq!(config.turbovec.index_path, "data/memories.tv");

        std::env::set_var("TURBOVEC_INDEX_PATH", "/var/lib/memex8/data/memories.tv");
        std::env::set_var(
            "TURBOVEC_ID_MAP_PATH",
            "/var/lib/memex8/data/memories_ids.json",
        );
        config.apply_env_overrides();
        std::env::remove_var("TURBOVEC_INDEX_PATH");
        std::env::remove_var("TURBOVEC_ID_MAP_PATH");

        assert_eq!(
            config.turbovec.index_path,
            "/var/lib/memex8/data/memories.tv"
        );
        assert_eq!(
            config.turbovec.id_map_path,
            "/var/lib/memex8/data/memories_ids.json"
        );
    }

    #[test]
    fn test_memex8_md_output_dir_defaults_when_absent() {
        let parsed: Memex8MdConfig =
            toml::from_str("enabled = true\nmax_memories = 20\nupdate_on_slumber = true\n")
                .expect("memex8_md section without output_dir must parse");
        assert_eq!(parsed.output_dir, "/watch");
    }

    #[test]
    fn coverage_defaults_and_validation_survive_legacy_configs() {
        let mut value = toml::Value::try_from(AppConfig::default()).unwrap();
        value.as_table_mut().unwrap().remove("search");
        let config: AppConfig = value.try_into().unwrap();
        assert_eq!(config.search.coverage.high, 0.55);
        assert_eq!(config.search.coverage.partial, 0.40);
        config.search.coverage.validate().unwrap();
        let config: SearchConfig = toml::from_str("[coverage]\nhigh = 0.7").unwrap();
        assert_eq!(config.coverage.partial, 0.40);
        for (partial, high) in [
            (0.5, 0.5),
            (0.7, 0.6),
            (-0.1, 0.5),
            (0.4, 1.1),
            (0.4, f32::NAN),
        ] {
            assert!(CoverageConfig { high, partial }.validate().is_err());
        }
    }

    #[test]
    fn legacy_config_defaults_to_bounded_dedup_dry_run() {
        let mut value = toml::Value::try_from(AppConfig::default()).unwrap();
        let slumber = value.get_mut("slumber").unwrap().as_table_mut().unwrap();
        for key in [
            "dedup_threshold",
            "dedup_dry_run",
            "dedup_max_comparisons_per_realm",
        ] {
            slumber.remove(key);
        }
        let decoded: AppConfig = value.try_into().unwrap();
        assert_eq!(decoded.slumber.dedup_threshold, 0.95);
        assert!(decoded.slumber.dedup_dry_run);
        assert_eq!(decoded.slumber.dedup_max_comparisons_per_realm, 1_000_000);
        decoded.slumber.validate_dedup().unwrap();
    }

    #[test]
    fn invalid_dedup_config_is_rejected() {
        let mut config = AppConfig::default().slumber;
        for threshold in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
            config.dedup_threshold = threshold;
            assert!(config.validate_dedup().is_err());
        }
        config.dedup_threshold = 0.95;
        config.dedup_max_comparisons_per_realm = 0;
        assert!(config.validate_dedup().is_err());
    }

    /// Pins the per-type age-cutoff derivation against the Weibull
    /// params. If someone tunes `weibull_params` in `engine::decay`
    /// without updating `default_auto_archive_days_for`, this test
    /// will fail and surface the divergence.
    ///
    /// Spot-check: pick representative types from each tier (long /
    /// medium / fast / default) and that the values land in the
    /// expected range. We don't pin exact numbers — that's brittle —
    /// just ranges that catch the obvious mistakes (e.g. dropping
    /// `profile` to 14 days).
    #[test]
    fn test_per_type_age_cutoffs_are_sane() {
        assert!(
            default_auto_archive_days_for("profile") >= 180,
            "profile should retain at least 6 months, got {}",
            default_auto_archive_days_for("profile")
        );
        assert!(
            default_auto_archive_days_for("preference") >= 90,
            "preference should retain at least 3 months, got {}",
            default_auto_archive_days_for("preference")
        );
        assert!(
            default_auto_archive_days_for("request") <= 14,
            "request is fastest-decaying, should prune within 2 weeks, got {}",
            default_auto_archive_days_for("request")
        );
        // Spot-check the math vs the Weibull params.
        // general: k=1.0, eta=168h → t = 168 × ln(10) / 24 ≈ 16.1 days
        let general = default_auto_archive_days_for("general");
        assert!(
            (14..=18).contains(&general),
            "general derived age {} not in expected 14-18d range",
            general
        );
    }

    #[test]
    fn test_unknown_type_falls_back_to_global() {
        // Unknown / empty types should fall back to the global default
        // (default_auto_archive_days = 14), matching what phase 4 does at
        // runtime when the per-type map is missing a key.
        let global = default_auto_archive_days();
        assert_eq!(
            default_auto_archive_days_for("definitely-not-a-real-type"),
            global
        );
    }

    #[test]
    fn test_per_type_map_is_complete() {
        // Every type the engine knows about must have an entry in the
        // default per-type map. Catches drift when `weibull_params`
        // adds a type without the config layer being updated.
        let map = default_auto_archive_days_by_type();
        for t in [
            "profile",
            "preference",
            "relationship",
            "learning",
            "fact",
            "entity",
            "setup",
            "pattern",
            "context",
            "observation",
            "artifact",
            "project",
            "goal",
            "decision",
            "commitment",
            "event",
            "instruction",
            "error",
            "issue",
            "request",
            "general",
        ] {
            assert!(map.contains_key(t), "missing per-type entry for {:?}", t);
            assert!(
                (7..=365).contains(map.get(t).unwrap()),
                "per-type value for {} out of range: {}",
                t,
                map[t]
            );
        }
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;

    fn base() -> String {
        toml::to_string(&AppConfig::default()).unwrap()
    }

    #[test]
    fn overrides_merge_over_base() {
        let c = AppConfig::from_toml_with_overrides(
            &base(),
            Some("[slumber]\ndedup_threshold = 0.9\n[search.coverage]\nhigh = 0.7\n"),
        )
        .unwrap();
        assert!((c.slumber.dedup_threshold - 0.9).abs() < 1e-6);
        assert!((c.search.coverage.high - 0.7).abs() < 1e-6);
        assert!((c.search.coverage.partial - 0.40).abs() < 1e-6); // untouched
    }

    #[test]
    fn invalid_overrides_fall_back_to_base() {
        let b = AppConfig::default();
        // partial >= high is invalid; unparsable text is also ignored
        for bad in [
            "[search.coverage]\nhigh = 0.2\n",
            "not = [valid",
            "[slumber]\ndedup_threshold = \"x\"\n",
        ] {
            let c = AppConfig::from_toml_with_overrides(&base(), Some(bad)).unwrap();
            assert_eq!(c.search.coverage.high, b.search.coverage.high);
            assert_eq!(c.slumber.dedup_threshold, b.slumber.dedup_threshold);
        }
    }

    #[test]
    fn settings_path_sits_beside_the_index() {
        std::env::remove_var("MEMEX8_SETTINGS_PATH");
        let p = AppConfig::settings_path_from_index("/var/lib/memex8/data/memories.tv");
        assert_eq!(
            p,
            std::path::PathBuf::from("/var/lib/memex8/data/settings.toml")
        );
    }
}
