use crate::engine::Engine;
use std::fs;
use std::path::{Path, PathBuf};

/// Default backup directory.
const DEFAULT_BACKUP_DIR: &str = "~/memex8-backups";

/// Maximum number of backups to keep (rotation).
const MAX_BACKUPS: usize = 7;

/// Backup all memories, realms, and graph edges to a timestamped tarball.
/// Returns the path to the created backup file.
pub async fn backup(engine: &Engine, output_path: Option<&str>) -> anyhow::Result<String> {
    let backup_dir = expand_path(output_path.unwrap_or(DEFAULT_BACKUP_DIR));
    fs::create_dir_all(&backup_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("memex8_backup_{}.tar.gz", timestamp);
    let backup_path = backup_dir.join(&filename);

    // Create a temp directory for the backup contents
    let temp_dir = std::env::temp_dir().join(format!("memex8_backup_{}", timestamp));
    fs::create_dir_all(&temp_dir)?;

    // Export all collections to JSON
    tracing::info!("Exporting memories...");
    export_memories(engine, &temp_dir).await?;

    tracing::info!("Exporting realms...");
    export_realms(engine, &temp_dir).await?;

    tracing::info!("Exporting graph edges...");
    export_graph_edges(engine, &temp_dir).await?;

    // Export config
    let config_json = serde_json::to_string_pretty(&engine.config())?;
    fs::write(temp_dir.join("config.json"), config_json)?;

    // Create tarball
    tracing::info!("Creating tarball...");
    create_tarball(&temp_dir, &backup_path)?;

    // Clean up temp directory
    let _ = fs::remove_dir_all(&temp_dir);

    // Rotate old backups
    rotate_backups(&backup_dir, MAX_BACKUPS)?;

    let count = count_memories(engine).await?;
    tracing::info!(
        "Backup complete: {} memories → {} ({})",
        count,
        backup_path.display(),
        format_bytes(fs::metadata(&backup_path)?.len())
    );

    Ok(backup_path.display().to_string())
}

/// Restore memories, realms, and graph edges from a backup tarball.
pub async fn restore(engine: &Engine, backup_path: &str, force: bool) -> anyhow::Result<usize> {
    let path = expand_path_buf(backup_path);
    if !path.exists() {
        anyhow::bail!("Backup file not found: {}", path.display());
    }

    if !force {
        let count = count_memories(engine).await?;
        if count > 0 {
            println!("⚠️  You have {} existing memories. Restore will ADD to them.", count);
            println!("   Use --force to skip this confirmation.");
            println!("Proceed? [y/N] ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                anyhow::bail!("Restore cancelled.");
            }
        }
    }

    // Extract tarball to temp directory
    let temp_dir = std::env::temp_dir().join("memex8_restore");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir)?;

    tracing::info!("Extracting backup...");
    extract_tarball(&path, &temp_dir)?;

    let mut restored = 0;

    // Import memories
    let memories_path = temp_dir.join("memories.json");
    if memories_path.exists() {
        let content = fs::read_to_string(&memories_path)?;
        let memories: Vec<crate::storage::qdrant::MemoryWithVector> =
            serde_json::from_str(&content)?;

        for m in &memories {
            let realm_id = m.memory.realm_id.as_deref().unwrap_or("");
            let reaction_score = crate::engine::reactions::infer_reaction(&m.memory.content);
            engine
                .store()
                .store_memory(
                    &m.memory.id,
                    &m.vector,
                    &m.memory.content,
                    m.memory.heading.as_deref(),
                    m.memory.source_file.as_deref(),
                    realm_id,
                    &m.memory.realm_name,
                    &m.memory.source_hash,
                    &m.memory.chunk_type,
                    reaction_score,
                )
                .await?;
            restored += 1;
        }
        tracing::info!("Restored {} memories", memories.len());
    }

    // Import realms
    let realms_path = temp_dir.join("realms.json");
    if realms_path.exists() {
        let content = fs::read_to_string(&realms_path)?;
        let realms: Vec<crate::storage::qdrant::RealmPoint> = serde_json::from_str(&content)?;
        for r in &realms {
            engine
                .store()
                .store_realm(&r.id, &r.centroid, &r.name, r.description.as_deref(), r.is_user_pinned)
                .await?;
        }
        tracing::info!("Restored {} realms", realms.len());
    }

    // Import graph edges
    let edges_path = temp_dir.join("graph_edges.json");
    if edges_path.exists() {
        let content = fs::read_to_string(&edges_path)?;
        let edges: Vec<crate::storage::qdrant::GraphEdge> = serde_json::from_str(&content)?;
        for e in &edges {
            engine.store().store_graph_edge(e).await?;
        }
        tracing::info!("Restored {} graph edges", edges.len());
    }

    // Clean up
    let _ = fs::remove_dir_all(&temp_dir);

    tracing::info!("Restore complete: {} items restored from {}", restored, path.display());
    Ok(restored)
}

/// List available backups sorted by date (newest first).
pub fn list_backups(backup_dir: Option<&str>) -> anyhow::Result<Vec<BackupInfo>> {
    let dir = expand_path(backup_dir.unwrap_or(DEFAULT_BACKUP_DIR));
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut backups = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "gz").unwrap_or(false)
            && path.file_name().map_or(false, |n| {
                n.to_string_lossy().starts_with("memex8_backup_")
            })
        {
            let metadata = path.metadata()?;
            backups.push(BackupInfo {
                path: path.display().to_string(),
                size: metadata.len(),
                created: metadata.modified()?.into(),
            });
        }
    }

    backups.sort_by(|a, b| b.created.cmp(&a.created));
    Ok(backups)
}

#[derive(Debug, Clone)]
pub struct BackupInfo {
    pub path: String,
    pub size: u64,
    pub created: std::time::SystemTime,
}

// ─── Internal helpers ──────────────────────────────────────────────────────────

fn expand_path(path: &str) -> PathBuf {
    if path.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(path.replacen('~', &home, 1))
    } else {
        PathBuf::from(path)
    }
}

fn expand_path_buf(path: &str) -> PathBuf {
    expand_path(path)
}

async fn export_memories(engine: &Engine, dir: &Path) -> anyhow::Result<()> {
    let memories = engine.store().scroll_all_memories_with_vectors().await?;
    let json = serde_json::to_string_pretty(&memories)?;
    fs::write(dir.join("memories.json"), json)?;
    Ok(())
}

async fn export_realms(engine: &Engine, dir: &Path) -> anyhow::Result<()> {
    let realms = engine.store().list_realms().await?;
    let json = serde_json::to_string_pretty(&realms)?;
    fs::write(dir.join("realms.json"), json)?;
    Ok(())
}

async fn export_graph_edges(engine: &Engine, dir: &Path) -> anyhow::Result<()> {
    let edges = engine.store().get_all_graph_edges().await?;
    let json = serde_json::to_string_pretty(&edges)?;
    fs::write(dir.join("graph_edges.json"), json)?;
    Ok(())
}

async fn count_memories(engine: &Engine) -> anyhow::Result<usize> {
    let stats = engine
        .store()
        .get_collection_stats(&engine.config().qdrant.collection_memories)
        .await?;
    Ok(stats.vector_count as usize)
}

fn create_tarball(source_dir: &Path, output: &Path) -> anyhow::Result<()> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::Builder;

    let file = fs::File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(encoder);
    tar.append_dir_all(".", source_dir)?;
    tar.finish()?;
    Ok(())
}

fn extract_tarball(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = fs::File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive.unpack(dest)?;
    Ok(())
}

fn rotate_backups(dir: &Path, max: usize) -> anyhow::Result<()> {
    let backups = list_backups(Some(&dir.display().to_string()))?;
    if backups.len() <= max {
        return Ok(());
    }

    for backup in backups.iter().skip(max) {
        let path = Path::new(&backup.path);
        if path.exists() {
            fs::remove_file(path)?;
            tracing::info!("Rotated old backup: {}", path.display());
        }
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
