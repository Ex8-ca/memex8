use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawChunk {
    pub content: String,
    pub heading: Option<String>,
    pub source_file: String,
    pub source_hash: String,
    pub parent_context: Option<String>,
    pub chunk_type: String,
}

pub struct Ingester {
    config: AppConfig,
}

impl Ingester {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn ingest_path(&self, path: &str, chunk_by: &str) -> anyhow::Result<Vec<RawChunk>> {
        let p = std::path::Path::new(path);
        if p.is_file() {
            self.ingest_file(p, chunk_by).await
        } else if p.is_dir() {
            self.ingest_directory(p, chunk_by).await
        } else {
            anyhow::bail!("Path not found: {}", path)
        }
    }

    async fn ingest_file(
        &self,
        path: &std::path::Path,
        chunk_by: &str,
    ) -> anyhow::Result<Vec<RawChunk>> {
        let content = std::fs::read_to_string(path)?;
        let source_hash = Self::hash_content(&content);
        let source_file = path.to_string_lossy().to_string();

        let chunks = crate::engine::chunker::chunk(&content, chunk_by, self.config.ingest.max_chunk_tokens)?;

        Ok(chunks
            .into_iter()
            .map(|c| RawChunk {
                content: c.content,
                heading: c.heading,
                source_file: source_file.clone(),
                source_hash: source_hash.clone(),
                parent_context: c.parent_heading,
                chunk_type: chunk_by.to_string(),
            })
            .collect())
    }

    async fn ingest_directory(
        &self,
        dir: &std::path::Path,
        chunk_by: &str,
    ) -> anyhow::Result<Vec<RawChunk>> {
        let mut all_chunks = Vec::new();
        for entry in walkdir(dir) {
            if entry.extension().map(|e| e == "md").unwrap_or(false) {
                match self.ingest_file(&entry, chunk_by).await {
                    Ok(chunks) => all_chunks.extend(chunks),
                    Err(e) => tracing::warn!("Failed to ingest {:?}: {}", entry, e),
                }
            }
        }
        Ok(all_chunks)
    }

    pub async fn start_watching(&self, path: &str) -> anyhow::Result<()> {
        // TODO: use notify crate for real-time file watching
        tracing::info!("File watching not yet implemented for: {}", path);
        Ok(())
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip hidden dirs and common non-content dirs
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') || name == "node_modules" || name == "target" {
                        continue;
                    }
                }
                files.extend(walkdir(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}
