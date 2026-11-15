use crate::engine::Engine;
use std::fs;
use std::path::{Path, PathBuf};
#[path = "backup_validation.rs"]
mod validation;
use validation::{validate_archive, write_manifest};

/// Default backup directory.
const DEFAULT_BACKUP_DIR: &str = "~/memex8-backups";

/// Maximum number of backups to keep (rotation).
const MAX_BACKUPS: usize = 7;

/// Backup memories, realms, graph edges, and consolidation journals to a timestamped tarball.
/// Returns the path to the created backup file.
pub async fn backup(engine: &Engine, output_path: Option<&str>) -> anyhow::Result<String> {
    let _guard = engine.store().maintenance_guard().await;
    let backup_dir = expand_path(output_path.unwrap_or(DEFAULT_BACKUP_DIR));
    fs::create_dir_all(&backup_dir)?;

    let timestamp = format!(
        "{}_{}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        uuid::Uuid::new_v4()
    );
    let filename = format!("memex8_backup_{}.tar.gz", timestamp);
    let backup_path = backup_dir.join(&filename);

    // Create a temp directory for the backup contents
    let workspace = tempfile::Builder::new()
        .prefix("memex8_backup_")
        .tempdir()?;
    let temp_dir = workspace.path().to_path_buf();

    // Export all collections to JSON
    tracing::info!("Exporting memories...");
    export_memories(engine, &temp_dir).await?;

    tracing::info!("Exporting realms...");
    export_realms(engine, &temp_dir).await?;

    tracing::info!("Exporting graph edges...");
    export_graph_edges(engine, &temp_dir).await?;

    let journals = engine.store().list_consolidations().await?;
    fs::write(
        temp_dir.join("consolidations.json"),
        serde_json::to_vec_pretty(&journals)?,
    )?;

    // Export config
    let config_json = serde_json::to_string_pretty(&engine.config())?;
    fs::write(temp_dir.join("config.json"), config_json)?;
    let evidence = engine.evidence_store()?;
    if evidence.is_some() {
        anyhow::ensure!(std::env::var("MEMEX8_BACKUP_WRITERS_STOPPED").as_deref() == Ok("1"), "paired logical backup requires stopped writers: stop all API/CLI/background/direct-store writers, then explicitly set MEMEX8_BACKUP_WRITERS_STOPPED=1; maintenance lock does not cover all writers");
    }
    if let Some(store) = &evidence {
        store.snapshot(&temp_dir.join("evidence.sqlite3"))?;
        let key = crate::storage::evidence::read_replay_key(&engine.evidence_path())?;
        write_private_key(&temp_dir.join("evidence.replay-key"), &key)?;
    }
    write_manifest(&temp_dir, evidence.is_some())?;
    let memories: Vec<crate::storage::qdrant::MemoryWithVector> =
        serde_json::from_slice(&fs::read(temp_dir.join("memories.json"))?)?;
    validate_archive(&temp_dir, &memories)?;

    // Create tarball
    tracing::info!("Creating tarball...");
    let partial_path = backup_path.with_extension("partial");
    create_tarball(&temp_dir, &partial_path)?;
    fs::rename(&partial_path, &backup_path)?;

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
    let _guard = engine.store().maintenance_guard().await;
    let destination_evidence = engine.evidence_path();
    let restore_marker = destination_evidence.with_extension("restore-in-progress");
    anyhow::ensure!(!restore_marker.exists(), "previous restore may be partial; discard BOTH target stores and restore marker, then retry on fresh empty stores");
    if destination_evidence.exists() {
        let conn = rusqlite::Connection::open_with_flags(
            &destination_evidence,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let records: bool =
            conn.query_row("SELECT EXISTS(SELECT 1 FROM sources)", [], |r| r.get(0))?;
        anyhow::ensure!(!records, "evidence restore requires an empty destination; discard partial destination and retry on fresh stores");
    }
    let path = expand_path_buf(backup_path);
    if !path.exists() {
        anyhow::bail!("Backup file not found: {}", path.display());
    }

    if !force {
        let count = count_memories(engine).await?;
        if count > 0 {
            println!(
                "⚠️  You have {} existing memories. Restore will ADD to them.",
                count
            );
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
    let workspace = tempfile::Builder::new()
        .prefix("memex8_restore_")
        .tempdir()?;
    let temp_dir = workspace.path().to_path_buf();

    tracing::info!("Extracting backup...");
    extract_tarball(&path, &temp_dir)?;

    // Validate all required exports before the first database write.
    let memories: Vec<crate::storage::qdrant::MemoryWithVector> =
        serde_json::from_str(&fs::read_to_string(temp_dir.join("memories.json"))?)?;
    for snapshot in &memories {
        super::redaction::ensure_memory(&snapshot.memory)?;
    }
    let paired = validate_archive(&temp_dir, &memories)?;
    if paired {
        anyhow::ensure!(
            !crate::storage::evidence::replay_key_path(&destination_evidence).exists(),
            "evidence restore requires an empty replay-key destination"
        );
    }
    anyhow::ensure!(
        memories
            .iter()
            .all(|m| m.vector.len() == engine.embed_dimensions as usize),
        "backup vector dimensions incompatible with destination"
    );
    anyhow::ensure!(
        engine
            .store()
            .scroll_all_memories()
            .await?
            .iter()
            .all(|m| m.evidence_refs.is_empty()),
        "evidence restore requires empty destination"
    );
    let realms: Vec<crate::storage::qdrant::RealmPoint> =
        serde_json::from_str(&fs::read_to_string(temp_dir.join("realms.json"))?)?;
    anyhow::ensure!(
        realms
            .iter()
            .all(|r| r.centroid.len() == engine.embed_dimensions as usize),
        "realm dimensions incompatible with destination"
    );
    let _: Vec<crate::storage::qdrant::GraphEdge> =
        serde_json::from_str(&fs::read_to_string(temp_dir.join("graph_edges.json"))?)?;

    // Optional for backward compatibility with backups made before journals existed.
    if paired {
        anyhow::ensure!(engine.store().scroll_all_memories().await?.is_empty()
            && engine.store().list_realms().await?.is_empty()
            && engine.store().get_all_graph_edges().await?.is_empty()
            && engine.store().list_consolidations().await?.is_empty(),
            "evidence restore requires empty Qdrant destination; discard partial destination and retry on fresh stores");
    }
    let journals_path = temp_dir.join("consolidations.json");
    let journals: Vec<crate::storage::qdrant::ConsolidationOperation> = if journals_path.exists() {
        serde_json::from_slice(&fs::read(&journals_path)?)?
    } else {
        Vec::new()
    };
    // Preflight every journal before the first database write. Never downgrade a
    // progressed operation by restoring an older backup over it.
    for operation in &journals {
        anyhow::ensure!(
            operation.embedding_dimensions == engine.embed_dimensions as usize,
            "journal dimensions incompatible with destination"
        );
        super::redaction::ensure_clean(operation)?;
        anyhow::ensure!(
            operation.version == 1,
            "Unsupported consolidation journal version"
        );
        if let Some(existing) = engine
            .store()
            .get_consolidation(&operation.summary_id)
            .await?
        {
            anyhow::ensure!(
                serde_json::to_value(&existing)? == serde_json::to_value(operation)?,
                "Consolidation journal {} differs from backup; restore to an isolated instance",
                operation.summary_id
            );
        }
    }
    // Persist retry refusal even if failure occurs before any row reaches either store.
    if paired {
        if let Some(parent) = restore_marker.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&restore_marker)?
            .sync_all()?;
    }
    // Not atomic: a later write failure requires discarding BOTH target stores.
    let result: anyhow::Result<usize> = async {
        if paired {
            if let Some(parent) = destination_evidence.parent() {
                fs::create_dir_all(parent)?;
            }
            if temp_dir.join("evidence.replay-key").exists() {
                let key =
                    crate::storage::evidence::read_replay_key(&temp_dir.join("evidence.sqlite3"))?;
                write_private_key(
                    &crate::storage::evidence::replay_key_path(&destination_evidence),
                    &key,
                )?;
            }
            let source = rusqlite::Connection::open_with_flags(
                temp_dir.join("evidence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            source.backup(rusqlite::MAIN_DB, &destination_evidence, None)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination_evidence, fs::Permissions::from_mode(0o600))?;
            }
        }
        for operation in &journals {
            if engine
                .store()
                .get_consolidation(&operation.summary_id)
                .await?
                .is_none()
            {
                engine.store().archive_consolidation(operation).await?;
            }
        }
        let mut restored = 0;

        // Import memories
        let memories_path = temp_dir.join("memories.json");
        if memories_path.exists() {
            let content = fs::read_to_string(&memories_path)?;
            let memories: Vec<crate::storage::qdrant::MemoryWithVector> =
                serde_json::from_str(&content)?;

            for m in &memories {
                engine.store().restore_memory(m).await?;
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
                    .store_realm(
                        &r.id,
                        &r.centroid,
                        &r.name,
                        r.description.as_deref(),
                        r.is_user_pinned,
                    )
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

        tracing::info!(
            "Restore complete: {} items restored from {}",
            restored,
            path.display()
        );
        if paired {
            fs::remove_file(&restore_marker)?;
        }
        Ok(restored)
    }
    .await;
    result.map_err(|e| anyhow::anyhow!("restore write phase failed (cross-store restore is not atomic); discard BOTH target stores and retry on fresh empty stores: {e:#}"))
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
            && path
                .file_name()
                .map_or(false, |n| n.to_string_lossy().starts_with("memex8_backup_"))
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

fn write_private_key(path: &Path, key: &[u8; 32]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(key)?;
    file.sync_all()?;
    Ok(())
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

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(encoder);
    for name in [
        "memories.json",
        "realms.json",
        "graph_edges.json",
        "consolidations.json",
        "config.json",
        "manifest.json",
        "evidence.sqlite3",
        "evidence.replay-key",
    ] {
        let path = source_dir.join(name);
        if path.exists() {
            tar.append_path_with_name(path, name)?;
        }
    }
    let encoder = tar.into_inner()?;
    encoder.finish()?.sync_all()?;
    Ok(())
}

fn extract_tarball(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = fs::File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if entry.header().entry_type().is_dir() && path == Path::new(".") {
            continue;
        }
        let mut components = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir));
        let name = match components.next() {
            Some(std::path::Component::Normal(n)) => n
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid archive name"))?
                .to_owned(),
            _ => anyhow::bail!("unsafe archive path"),
        };
        anyhow::ensure!(
            components.next().is_none() && entry.header().entry_type().is_file(),
            "unsafe archive entry"
        );
        anyhow::ensure!(
            matches!(
                name.as_str(),
                "manifest.json"
                    | "evidence.sqlite3"
                    | "evidence.replay-key"
                    | "memories.json"
                    | "realms.json"
                    | "graph_edges.json"
                    | "consolidations.json"
                    | "config.json"
            ),
            "unexpected archive entry"
        );
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("archive size overflow"))?;
        anyhow::ensure!(
            size <= 512 * 1024 * 1024 && total <= 1024 * 1024 * 1024,
            "archive exceeds extraction limits (512 MiB/component, 1 GiB total)"
        );
        anyhow::ensure!(seen.insert(name.clone()), "duplicate archive entry");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(dest.join(name))?;
        std::io::copy(&mut entry, &mut output)?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_rejects_malformed_realm_and_graph_before_writes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("realms.json"), br#"[{"id":"not-a-uuid","name":"r","description":null,"centroid":[1.0],"memory_count":0,"is_user_pinned":false}]"#).unwrap();
        assert!(
            validate_archive(root.path(), &[]).is_err(),
            "invalid realm identity accepted"
        );
        fs::write(root.path().join("realms.json"), b"[]").unwrap();
        fs::write(root.path().join("graph_edges.json"), br#"[{"from_memory_id":"bad","to_memory_id":"bad","relation_type":"similar","weight":1.0}]"#).unwrap();
        assert!(
            validate_archive(root.path(), &[]).is_err(),
            "invalid graph identity accepted"
        );
    }

    #[test]
    fn extraction_rejects_oversized_header_before_creating_file() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("large.tar.gz");
        let mut header = tar::Header::new_gnu();
        header.set_path("memories.json").unwrap();
        header.set_size(512 * 1024 * 1024 + 1);
        header.set_mode(0o600);
        header.set_cksum();
        use std::io::Write;
        let mut gzip = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.finish().unwrap();
        let dest = root.path().join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_tarball(&archive, &dest).is_err());
        assert!(
            !dest.join("memories.json").exists(),
            "oversized entry created before refusal"
        );
    }

    #[test]
    fn preflight_rejects_invalid_journal_identity_and_dimensions() {
        let root = tempfile::tempdir().unwrap();
        let mut operation = serde_json::json!({"version":1,"summary_id":"bad","summary_hash":"x","created_at":"2026-01-01T00:00:00Z","state":"complete","preserve_originals":false,"embedding_provider":"test","embedding_model":"test","embedding_dimensions":8,"sources":[],"cooldown_until":null});
        fs::write(
            root.path().join("consolidations.json"),
            serde_json::to_vec(&serde_json::json!([operation])).unwrap(),
        )
        .unwrap();
        assert!(
            validate_archive(root.path(), &[]).is_err(),
            "invalid journal UUID accepted"
        );
        operation["summary_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        operation["embedding_dimensions"] = serde_json::json!(0);
        fs::write(
            root.path().join("consolidations.json"),
            serde_json::to_vec(&serde_json::json!([operation])).unwrap(),
        )
        .unwrap();
        assert!(
            validate_archive(root.path(), &[]).is_err(),
            "invalid journal dimensions accepted"
        );
    }

    #[test]
    fn restore_preflight_rejects_bad_vector_before_writes() {
        let root = tempfile::tempdir().unwrap();
        let m: crate::storage::qdrant::MemoryWithVector = serde_json::from_value(serde_json::json!({"vector":[], "id":"bad", "content":"x", "summary":null,"source_file":null,"realm_id":null,"realm_name":"", "importance":1.0,"upvotes":0,"tags":[],"ingested_at":"2026-01-01T00:00:00Z","last_accessed":"2026-01-01T00:00:00Z","access_count":0,"chunk_type":"manual","heading":null,"source_hash":""})).unwrap();
        assert!(validate_archive(root.path(), &[m]).is_err());
    }

    #[test]
    fn paired_archive_refuses_extra_sqlite_schema_objects() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "memories.json",
            "realms.json",
            "graph_edges.json",
            "consolidations.json",
        ] {
            fs::write(root.path().join(name), b"[]").unwrap();
        }
        fs::write(root.path().join("config.json"), b"{}").unwrap();
        let path = root.path().join("evidence.sqlite3");
        crate::storage::evidence::EvidenceStore::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE unexpected(secret TEXT)")
            .unwrap();
        drop(conn);
        write_manifest(root.path(), true).unwrap();
        assert!(validate_archive(root.path(), &[]).is_err());
    }

    #[test]
    fn legacy_archive_refuses_evidence_in_recovery_journal() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("consolidations.json"),
            br#"[{"sources":[{"evidence_refs":[{"source_id":"missing"}]}]}]"#,
        )
        .unwrap();
        assert!(validate_archive(root.path(), &[]).is_err());
    }

    #[test]
    fn legacy_archive_without_evidence_is_supported() {
        let root = tempfile::tempdir().unwrap();
        assert!(!validate_archive(root.path(), &[]).unwrap());
    }

    #[test]
    fn paired_manifest_requires_intact_sqlite_and_supported_schema() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "memories.json",
            "realms.json",
            "graph_edges.json",
            "consolidations.json",
        ] {
            fs::write(root.path().join(name), b"[]").unwrap();
        }
        fs::write(root.path().join("config.json"), b"{}").unwrap();
        let ledger =
            crate::storage::evidence::EvidenceStore::open(&root.path().join("evidence.sqlite3"))
                .unwrap();
        // The ledger already created its private key beside evidence.sqlite3.
        let _key = crate::storage::evidence::read_replay_key(&root.path().join("evidence.sqlite3"))
            .unwrap();
        ledger.snapshot(&root.path().join("snapshot")).unwrap();
        fs::rename(
            root.path().join("snapshot"),
            root.path().join("evidence.sqlite3"),
        )
        .unwrap();
        write_manifest(root.path(), true).unwrap();
        assert!(validate_archive(root.path(), &[]).unwrap());
        let original_key =
            crate::storage::evidence::read_replay_key(&root.path().join("evidence.sqlite3"))
                .unwrap();
        fs::remove_file(root.path().join("evidence.replay-key")).unwrap();
        assert!(
            validate_archive(root.path(), &[]).is_err(),
            "keyless archive accepted"
        );
        write_private_key(&root.path().join("evidence.replay-key"), &original_key).unwrap();
        write_manifest(root.path(), true).unwrap();
        let archive = root.path().join("paired.tar.gz");
        create_tarball(root.path(), &archive).unwrap();
        let recovered = root.path().join("recovered");
        fs::create_dir(&recovered).unwrap();
        extract_tarball(&archive, &recovered).unwrap();
        assert!(validate_archive(&recovered, &[]).unwrap());
        assert_eq!(
            crate::storage::evidence::read_replay_key(&recovered.join("evidence.sqlite3")).unwrap(),
            original_key
        );
        let conn = rusqlite::Connection::open(root.path().join("evidence.sqlite3")).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        write_manifest(root.path(), true).unwrap();
        assert!(validate_archive(root.path(), &[]).is_err());
        fs::write(root.path().join("evidence.sqlite3"), b"corrupt").unwrap();
        assert!(validate_archive(root.path(), &[]).is_err());
        fs::remove_file(root.path().join("evidence.sqlite3")).unwrap();
        assert!(validate_archive(root.path(), &[]).is_err());
    }

    #[test]
    fn paired_archive_round_trip_preserves_erased_key_barrier() {
        use crate::storage::evidence::{CaptureRequest, EvidenceStore};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("evidence.sqlite3");
        let store = EvidenceStore::open(&path).unwrap();
        let capture = CaptureRequest {
            speaker: "user".into(),
            origin: "explicit_rest".into(),
            pre_redacted: true,
            agent: None,
            device: None,
            session: None,
        };
        let row = store
            .prepare("x", "secret", &capture, &[], "private")
            .unwrap();
        store.begin_erase(&row.memory_id).unwrap();
        let source = root.path().join("export");
        fs::create_dir(&source).unwrap();
        for name in [
            "memories.json",
            "realms.json",
            "graph_edges.json",
            "consolidations.json",
        ] {
            fs::write(source.join(name), b"[]").unwrap();
        }
        fs::write(source.join("config.json"), b"{}").unwrap();
        store.snapshot(&source.join("evidence.sqlite3")).unwrap();
        write_private_key(
            &source.join("evidence.replay-key"),
            &crate::storage::evidence::read_replay_key(&path).unwrap(),
        )
        .unwrap();
        write_manifest(&source, true).unwrap();
        assert!(validate_archive(&source, &[]).unwrap());
        let archive = root.path().join("paired.tar.gz");
        create_tarball(&source, &archive).unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        extract_tarball(&archive, &target).unwrap();
        assert!(validate_archive(&target, &[]).unwrap());
        let restored = EvidenceStore::open(&target.join("evidence.sqlite3")).unwrap();
        assert!(restored
            .prepare("x", "changed", &capture, &[], "private")
            .is_err());
        fs::remove_file(target.join("evidence.replay-key")).unwrap();
        assert!(validate_archive(&target, &[]).is_err());
    }

    #[test]
    fn extraction_rejects_unknown_authoritative_entries() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("surprise.sqlite3"), b"sensitive").unwrap();
        let archive = root.path().join("archive.tar.gz");
        {
            let file = fs::File::create(&archive).unwrap();
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::default(),
            ));
            builder
                .append_path_with_name(source.join("surprise.sqlite3"), "surprise.sqlite3")
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        assert!(extract_tarball(&archive, &target).is_err());
    }

    #[test]
    fn archive_finishes_gzip_and_round_trips_contents() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let restored = root.path().join("restored");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&restored).unwrap();
        let original = br#"[{"id":"example","tags":["project:demo"],"importance":0.42}]"#;
        fs::write(source.join("memories.json"), original).unwrap();
        let archive = root.path().join("backup.tar.gz");
        create_tarball(&source, &archive).unwrap();
        extract_tarball(&archive, &restored).unwrap();
        assert_eq!(fs::read(restored.join("memories.json")).unwrap(), original);
    }
}
