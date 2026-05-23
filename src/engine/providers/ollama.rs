use crate::engine::embedder::Embedder;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

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
    max_concurrent: usize,
}

impl OllamaEmbedder {
    pub fn new(url: &str, model: &str, dimensions: u32, max_concurrent: usize) -> anyhow::Result<Self> {
        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
            max_concurrent,
        })
    }

    /// Embed a single text with retry logic for transient errors (429, 503).
    /// Retries up to 3 times with exponential backoff (1s, 2s, 4s).
    async fn embed_with_retry(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Self::embed_single_with_retry(&self.client, &self.url, &self.model, text).await
    }

    /// Static helper: embed a single text with retry logic, takes raw params for task spawning.
    async fn embed_single_with_retry(
        client: &reqwest::Client,
        url: &str,
        model: &str,
        text: &str,
    ) -> anyhow::Result<Vec<f32>> {
        let mut last_err = None;
        for attempt in 0..=3 {
            match Self::embed_inner(client, url, model, text).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("(429)") || msg.contains("(503)") {
                        last_err = Some(e);
                        if attempt < 3 {
                            let delay = 1u64 << attempt; // 1s, 2s, 4s
                            tracing::warn!(
                                "Ollama embed retryable error (attempt {}/3), backing off {}s: {}",
                                attempt + 1,
                                delay,
                                msg
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Ollama embed failed after retries")))
    }

    /// Inner embed call without retry logic.
    async fn embed_inner(
        client: &reqwest::Client,
        url: &str,
        model: &str,
        text: &str,
    ) -> anyhow::Result<Vec<f32>> {
        let resp = client
            .post(format!("{}/api/embed", url))
            .json(&OllamaRequest {
                model: model.to_string(),
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
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed_with_retry(text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let total = texts.len();
        let mut completed: usize = 0;
        let mut errors: usize = 0;
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        let mut results: Vec<Option<anyhow::Result<Vec<f32>>>> = (0..total).map(|_| None).collect();

        // Process texts concurrently, bounded by semaphore
        let mut set = tokio::task::JoinSet::new();

        for (i, text) in texts.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await?;
            let client = self.client.clone();
            let url = self.url.clone();
            let model = self.model.clone();
            let idx = i;
            let text = text.to_string();

            set.spawn(async move {
                let result = Self::embed_single_with_retry(&client, &url, &model, &text).await;
                drop(permit);
                (idx, result)
            });
        }

        // Collect results in order
        while let Some(result) = set.join_next().await {
            match result {
                Ok((idx, Ok(embedding))) => {
                    results[idx] = Some(Ok(embedding));
                    completed += 1;
                }
                Ok((idx, Err(e))) => {
                    tracing::error!("Ollama embed failed for item {}: {}", idx, e);
                    results[idx] = Some(Err(e));
                    errors += 1;
                }
                Err(join_err) => {
                    tracing::error!("Ollama embed task panicked: {}", join_err);
                    errors += 1;
                }
            }
        }

        // Fallback: if ALL items failed, retry sequentially
        if errors == total {
            tracing::warn!(
                "All {} concurrent embeds failed, falling back to sequential processing",
                total
            );
            let mut seq_results = Vec::with_capacity(total);
            let mut seq_errors = 0usize;
            for (i, text) in texts.iter().enumerate() {
                match self.embed_with_retry(text).await {
                    Ok(v) => seq_results.push(v),
                    Err(e) => {
                        tracing::error!("Sequential fallback also failed for item {}: {}", i, e);
                        seq_errors += 1;
                    }
                }
            }
            // If sequential also all failed, return the original error
            if seq_results.is_empty() {
                anyhow::bail!("All embed requests failed even after sequential fallback");
            }
            tracing::info!(
                "Sequential fallback: {} succeeded, {} failed",
                seq_results.len(),
                seq_errors
            );
            return Ok(seq_results);
        }

        // Flatten results, filtering out failed items
        let mut final_results = Vec::with_capacity(total - errors);
        for (i, result) in results.into_iter().enumerate() {
            match result {
                Some(Ok(v)) => final_results.push(v),
                Some(Err(_)) => {
                    // Item failed; skip it (per-item error handling)
                }
                None => {
                    tracing::error!("Missing result for item {} (should not happen)", i);
                }
            }
        }

        tracing::info!(
            "Ollama embed_batch: total={}, completed={}, errors={}",
            total, completed, errors
        );
        Ok(final_results)
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
}
