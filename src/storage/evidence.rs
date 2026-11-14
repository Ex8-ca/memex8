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
impl EvidenceStore {
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
        let conn = store.connection()?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version <= 1, "unsupported evidence schema version");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS sources (
                source_id TEXT PRIMARY KEY, memory_id TEXT NOT NULL UNIQUE,
                idempotency_key TEXT NOT NULL UNIQUE, request TEXT NOT NULL,
                record TEXT NOT NULL);
            PRAGMA user_version=1;",
        )?;
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
}
