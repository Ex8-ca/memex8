use crate::engine::embedder::Embedder;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    input: serde_json::Value,
    dimensions: u32,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    embedding: Vec<f32>,
}

pub struct OpenAiEmbedder {
    api_key: String,
    model: String,
    dimensions: u32,
    client: reqwest::Client,
}

impl OpenAiEmbedder {
    pub fn new(api_key: &str, model: &str, dimensions: u32) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let resp = self.client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&OpenAiRequest {
                model: self.model.clone(),
                input: serde_json::Value::String(text.to_string()),
                dimensions: self.dimensions,
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await?;
            anyhow::bail!("OpenAI API error ({}): {}", status, body);
        }

        let result: OpenAiResponse = resp.json().await?;
        result.data.into_iter().next()
            .map(|d| d.embedding)
            .ok_or_else(|| anyhow::anyhow!("No embedding in OpenAI response"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let input: Vec<&str> = texts.to_vec();
        let resp = self.client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&serde_json::json!({
                "model": self.model,
                "input": input,
                "dimensions": self.dimensions,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await?;
            anyhow::bail!("OpenAI API error ({}): {}", status, body);
        }

        let result: OpenAiResponse = resp.json().await?;
        Ok(result.data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
}
