use crate::engine::embedder::Embedder;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    input: String,
}

#[derive(Deserialize)]
struct OllamaResponse {
    embeddings: Vec<Vec<f32>>,
}

pub struct OllamaEmbedder {
    url: String,
    model: String,
    dimensions: u32,
    client: reqwest::Client,
}

impl OllamaEmbedder {
    pub fn new(url: &str, model: &str, dimensions: u32) -> anyhow::Result<Self> {
        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let resp = self.client
            .post(format!("{}/api/embed", self.url))
            .json(&OllamaRequest {
                model: self.model.clone(),
                input: text.to_string(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await?;
            anyhow::bail!("Ollama API error ({}): {}", status, body);
        }

        let result: OllamaResponse = resp.json().await?;
        Ok(result.embeddings.into_iter().next().unwrap_or_default())
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
}
