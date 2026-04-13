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
    pub web: WebConfig,
    #[serde(default)]
    pub watch: Vec<WatchConfig>,
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
}

fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self { url: default_ollama_url() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiConfig {
    #[serde(default = "default_openai_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_openai_model")]
    pub model: String,
    #[serde(default = "default_openai_dims")]
    pub dimensions: u32,
}

fn default_openai_key_env() -> String { "OPENAI_API_KEY".into() }
fn default_openai_model() -> String { "text-embedding-3-small".into() }
fn default_openai_dims() -> u32 { 1536 }

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_openai_key_env(),
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
    #[serde(default = "default_quantized")]
    pub collection_quantized: String,
    #[serde(default = "default_realms")]
    pub collection_realms: String,
}

fn default_qdrant_url() -> String {
    std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".into())
}
fn default_memories() -> String { "memories".into() }
fn default_quantized() -> String { "quantized".into() }
fn default_realms() -> String { "realms".into() }

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            collection_memories: default_memories(),
            collection_quantized: default_quantized(),
            collection_realms: default_realms(),
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
    #[serde(default)]
    pub summarize: SummarizeConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memex8MdConfig {
    pub enabled: bool,
    pub max_memories: u32,
    pub update_on_slumber: bool,
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

fn default_chunk() -> String { "section".into() }
fn default_poll() -> String { "5m".into() }

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
                summarize: SummarizeConfig::default(),
            },
            memex8_md: Memex8MdConfig {
                enabled: true,
                max_memories: 20,
                update_on_slumber: true,
            },
            web: WebConfig {
                enabled: true,
                theme: "dark".into(),
            },
            watch: vec![],
        }
    }
}
