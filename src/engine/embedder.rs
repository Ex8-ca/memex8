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
    let inner: Box<dyn Embedder> = match config.embedding.provider.as_str() {
        "ollama" => Box::new(OllamaEmbedder::new(
            &config.embedding.ollama.url,
            &config.embedding.model,
            config.embedding.dimensions,
            config.embedding.ollama.max_concurrent,
        )?),
        "openai" | "openai-compatible" => {
            let api_key = config
                .openai_api_key()
                .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY not set"))?;
            Box::new(OpenAiEmbedder::new(
                &api_key,
                &config.embedding.openai.base_url,
                &config.embedding.openai.model,
                config.embedding.openai.dimensions,
            )?)
        }
        _ => anyhow::bail!(
            "Unknown embedding provider: {}. Supported: ollama, openai, openai-compatible",
            config.embedding.provider
        ),
    };
    Ok(Box::new(RedactingEmbedder(inner)))
}

struct RedactingEmbedder(Box<dyn Embedder>);
#[async_trait]
impl Embedder for RedactingEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.0.embed(&super::redaction::required(text)?).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let clean: Vec<String> = texts
            .iter()
            .map(|s| super::redaction::required(s))
            .collect::<Result<_, _>>()?;
        let refs: Vec<&str> = clean.iter().map(String::as_str).collect();
        self.0.embed_batch(&refs).await
    }
    fn dimensions(&self) -> u32 {
        self.0.dimensions()
    }
}
