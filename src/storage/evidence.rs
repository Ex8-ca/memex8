//! Local source ledger. SQLite commits precede Qdrant projection; retries use stable IDs.
//! No URL/path resolution. Content is caller-provided, pre-redacted UTF-8.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub speaker: String,
    pub origin: String,
    pub pre_redacted: bool,
    pub agent: Option<String>,
    pub device: Option<String>,
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub schema_version: u32,
    pub source_id: String,
    pub memory_id: String,
    pub content: String,
    pub content_hash: String,
    pub speaker: String,
    pub origin: String,
    pub agent: Option<String>,
    pub device: Option<String>,
    pub session: Option<String>,
    pub captured_at: String,
    pub scope: Vec<String>,
    pub visibility: String,
    pub redaction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub schema_version: u32,
    pub source_id: String,
    /// UTF-8 byte offsets over normalized, caller-redacted stored content.
    pub start_byte: usize,
    pub end_byte: usize,
}

pub struct EvidenceStore {
    path: PathBuf,
}

/// Durable replay barrier and Qdrant deletion work item. Never contains source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErasureIntent {
    pub source_id: String,
    pub memory_id: String,
    pub complete: bool,
}

impl EvidenceStore {
    /// Consistent logical SQLite snapshot, including committed WAL pages and pending prepares.
    pub fn snapshot(&self, destination: &Path) -> Result<()> {
        self.connection()?
            .backup(rusqlite::MAIN_DB, destination, None)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        let store = Self { path: path.into() };
        let mut conn = store.connection()?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version <= 2, "unsupported evidence schema version");
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sources (
                source_id TEXT PRIMARY KEY, memory_id TEXT NOT NULL UNIQUE,
                idempotency_key TEXT NOT NULL UNIQUE, request TEXT NOT NULL,
                record TEXT NOT NULL);",
        )?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS erasure_intents (
                memory_id TEXT PRIMARY KEY, source_id TEXT NOT NULL UNIQUE,
                key_digest TEXT NOT NULL UNIQUE, complete INTEGER NOT NULL DEFAULT 0
                    CHECK (complete IN (0, 1)));
            PRAGMA user_version=2;",
        )?;
        tx.commit()?;
        Ok(store)
    }
    fn connection(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(conn)
    }
    pub fn prepare(
        &self,
        key: &str,
        content: &str,
        capture: &CaptureRequest,
        scope: &[String],
        visibility: &str,
    ) -> Result<SourceRecord> {
        ensure!(
            !key.is_empty() && key.len() <= 128 && !key.chars().any(char::is_control),
            "invalid idempotency key"
        );
        ensure!(
            !content.trim().is_empty() && content.len() <= 65536,
            "evidence content must be 1..65536 bytes"
        );
        ensure!(
            matches!(capture.speaker.as_str(), "user" | "assistant" | "tool"),
            "invalid speaker"
        );
        ensure!(
            capture.origin == "explicit_rest",
            "unsupported evidence origin"
        );
        ensure!(
            capture.pre_redacted,
            "caller must supply pre-redacted content"
        );
        ensure!(
            matches!(visibility, "private" | "public"),
            "invalid visibility"
        );
        ensure!(
            scope.len() <= 32
                && scope
                    .iter()
                    .all(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)),
            "invalid scope tags"
        );
        ensure!(
            [&capture.agent, &capture.device, &capture.session]
                .iter()
                .all(|v| v.as_ref().is_none_or(|s| !s.is_empty()
                    && s.len() <= 256
                    && !s.chars().any(char::is_control))),
            "invalid source metadata"
        );
        let request = serde_json::to_string(&(content, capture, scope, visibility))?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        let tombstoned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM erasure_intents WHERE key_digest=?1)",
            [&digest],
            |r| r.get(0),
        )?;
        ensure!(!tombstoned, "idempotency key belongs to erased evidence");
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT request, record FROM sources WHERE idempotency_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((prior, record)) = existing {
            ensure!(
                prior == request,
                "idempotency key conflicts with prior request"
            );
            return Ok(serde_json::from_str(&record)?);
        }
        let content = content.replace("\r\n", "\n").replace('\r', "\n");
        let record = SourceRecord {
            schema_version: 1,
            source_id: uuid::Uuid::new_v4().to_string(),
            memory_id: uuid::Uuid::new_v4().to_string(),
            content_hash: format!("{:x}", Sha256::digest(content.as_bytes())),
            content,
            speaker: capture.speaker.clone(),
            origin: capture.origin.clone(),
            agent: capture.agent.clone(),
            device: capture.device.clone(),
            session: capture.session.clone(),
            captured_at: chrono::Utc::now().to_rfc3339(),
            scope: scope.to_vec(),
            visibility: visibility.into(),
            redaction: "caller_pre_redacted".into(),
        };
        tx.execute(
            "INSERT INTO sources VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.source_id,
                record.memory_id,
                key,
                request,
                serde_json::to_string(&record)?
            ],
        )?;
        tx.commit()?;
        Ok(record)
    }
    pub fn get(&self, source_id: &str) -> Result<Option<SourceRecord>> {
        let record: Option<String> = self
            .connection()?
            .query_row(
                "SELECT record FROM sources WHERE source_id=?1",
                [source_id],
                |r| r.get(0),
            )
            .optional()?;
        record.map(|r| Ok(serde_json::from_str(&r)?)).transpose()
    }
    pub fn contains_memory(&self, memory_id: &str) -> Result<bool> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE memory_id=?1)",
            [memory_id],
            |r| r.get(0),
        )?)
    }
    pub fn has_records(&self) -> Result<bool> {
        Ok(self
            .connection()?
            .query_row("SELECT EXISTS(SELECT 1 FROM sources)", [], |r| r.get(0))?)
    }

    /// Atomically hide source text, persist the replay barrier and queue projection deletion.
    /// Caller must serialize against capture/projection with the engine maintenance guard.
    pub fn begin_erase(&self, memory_id: &str) -> Result<Option<ErasureIntent>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let prior = tx
            .query_row(
                "SELECT source_id, memory_id, complete FROM erasure_intents WHERE memory_id=?1",
                [memory_id],
                |r| {
                    Ok(ErasureIntent {
                        source_id: r.get(0)?,
                        memory_id: r.get(1)?,
                        complete: r.get(2)?,
                    })
                },
            )
            .optional()?;
        if prior.is_some() {
            return Ok(prior);
        }
        let source: Option<(String, String)> = tx
            .query_row(
                "SELECT source_id, idempotency_key FROM sources WHERE memory_id=?1",
                [memory_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((source_id, key)) = source else {
            return Ok(None);
        };
        let key_digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        tx.execute(
            "INSERT INTO erasure_intents (memory_id, source_id, key_digest) VALUES (?1, ?2, ?3)",
            params![memory_id, source_id, key_digest],
        )?;
        tx.execute("DELETE FROM sources WHERE memory_id=?1", [memory_id])?;
        tx.commit()?;
        Ok(Some(ErasureIntent {
            source_id,
            memory_id: memory_id.into(),
            complete: false,
        }))
    }

    /// Mark an existing intent complete only after Qdrant confirms projection removal.
    pub fn finish_erase(&self, memory_id: &str) -> Result<()> {
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE erasure_intents SET complete=1 WHERE memory_id=?1",
            [memory_id],
        )?;
        ensure!(changed == 1, "no erasure intent for memory");
        Ok(())
    }

    /// Work remaining after a crash; the engine must delete these projections before serving.
    pub fn pending_erasures(&self) -> Result<Vec<ErasureIntent>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT source_id, memory_id, complete FROM erasure_intents WHERE complete=0 ORDER BY memory_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ErasureIntent {
                source_id: r.get(0)?,
                memory_id: r.get(1)?,
                complete: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
