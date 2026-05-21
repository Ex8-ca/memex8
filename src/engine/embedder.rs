use crate::config::AppConfig;
use crate::engine::providers::ollama::OllamaEmbedder;
use crate::engine::providers::openai::OpenAiEmbedder;
use async_trait::async_trait;

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> u32;
}

pub fn create_embedder(config: &AppConfig) -> anyhow::Result<Box<dyn Embedder>> {
    match config.embedding.provider.as_str() {
        "ollama" => Ok(Box::new(OllamaEmbedder::new(
            &config.embedding.ollama.url,
            &config.embedding.model,
            config.embedding.dimensions,
        )?)),
        "openai" | "openai-compatible" => {
            let api_key = config
                .openai_api_key()
                .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY not set"))?;
            Ok(Box::new(OpenAiEmbedder::new(
                &api_key,
                &config.embedding.openai.base_url,
                &config.embedding.openai.model,
                config.embedding.openai.dimensions,
            )?))
        }
        _ => anyhow::bail!("Unknown embedding provider: {}. Supported: ollama, openai, openai-compatible", config.embedding.provider),
    }
}
