//! Trusted logical archive preflight. Hashes detect corruption, not malicious replacement.
use crate::storage::{
    evidence::{CaptureRequest, SourceRecord},
    qdrant::MemoryWithVector,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    capabilities: Vec<String>,
    evidence_schema: Option<u32>,
    files: BTreeMap<String, String>,
}
const BASE: &[&str] = &[
    "memories.json",
    "realms.json",
    "graph_edges.json",
    "consolidations.json",
    "config.json",
];
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
pub(super) fn write_manifest(dir: &Path, evidence: bool) -> Result<()> {
    let mut files = BTreeMap::new();
    for name in BASE
        .iter()
        .copied()
        .chain(evidence.then_some("evidence.sqlite3"))
    {
        files.insert(name.to_owned(), digest(&dir.join(name))?);
    }
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&Manifest {
            version: 1,
            capabilities: vec![
                "logical-qdrant-v1".into(),
                "consolidation-dedup-v1".into(),
                "timeline-payload-v1".into(),
            ],
            evidence_schema: evidence.then_some(1),
            files,
        })?,
    )?;
    Ok(())
}
pub(super) fn validate_archive(dir: &Path, memories: &[MemoryWithVector]) -> Result<bool> {
    let mut ids = std::collections::HashSet::new();
    let dimensions = memories.first().map(|m| m.vector.len());
    for m in memories {
        ensure!(ids.insert(&m.memory.id), "duplicate memory identity");
        ensure!(
            Some(m.vector.len()) == dimensions,
            "inconsistent backup vector dimensions"
        );
        if let Some(realm) = &m.memory.realm_id {
            uuid::Uuid::parse_str(realm)?;
        }
        uuid::Uuid::parse_str(&m.memory.id)?;
        ensure!(
            !m.vector.is_empty() && m.vector.iter().all(|v| v.is_finite()),
            "invalid backup vector"
        );
    }
    if dir.join("realms.json").exists() {
        let realms: Vec<crate::storage::qdrant::RealmPoint> =
            serde_json::from_slice(&fs::read(dir.join("realms.json"))?)?;
        let mut realm_ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for realm in realms {
            uuid::Uuid::parse_str(&realm.id)?;
            ensure!(
                realm_ids.insert(realm.id) && names.insert(realm.name),
                "duplicate realm identity/name"
            );
            ensure!(
                !realm.centroid.is_empty()
                    && realm.centroid.iter().all(|v| v.is_finite())
                    && dimensions.is_none_or(|d| realm.centroid.len() == d),
                "invalid realm dimensions/vector"
            );
        }
    }
    if dir.join("graph_edges.json").exists() {
        let edges: Vec<crate::storage::qdrant::GraphEdge> =
            serde_json::from_slice(&fs::read(dir.join("graph_edges.json"))?)?;
        let mut edges_seen = std::collections::HashSet::new();
        for edge in edges {
            uuid::Uuid::parse_str(&edge.from_memory_id)?;
            uuid::Uuid::parse_str(&edge.to_memory_id)?;
            ensure!(
                edge.weight.is_finite()
                    && edges_seen.insert((edge.from_memory_id, edge.to_memory_id)),
                "invalid/duplicate graph edge"
            );
        }
    }
    if dir.join("consolidations.json").exists() {
        let journals: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("consolidations.json"))?)?;
        fn has_links(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::Object(map) => map.iter().any(|(k, v)| {
                    (k == "evidence_refs" && v.as_array().is_none_or(|a| !a.is_empty()))
                        || has_links(v)
                }),
                serde_json::Value::Array(a) => a.iter().any(has_links),
                _ => false,
            }
        }
        ensure!(
            !has_links(&journals),
            "evidence-linked derived recovery history unsupported"
        );
        let operations: Vec<crate::storage::qdrant::ConsolidationOperation> =
            serde_json::from_value(journals)?;
        let mut operation_ids = std::collections::HashSet::new();
        for operation in operations {
            uuid::Uuid::parse_str(&operation.summary_id)?;
            ensure!(
                operation_ids.insert(operation.summary_id.clone())
                    && operation.version == 1
                    && matches!(operation.kind.as_str(), "consolidation" | "dedup")
                    && matches!(
                        operation.state.as_str(),
                        "prepared" | "complete" | "restoring" | "restored"
                    ),
                "unsupported/duplicate recovery journal"
            );
            ensure!(
                operation.embedding_dimensions > 0
                    && dimensions.is_none_or(|d| d == operation.embedding_dimensions),
                "invalid journal dimensions"
            );
            let mut source_ids = std::collections::HashSet::new();
            for source in &operation.sources {
                uuid::Uuid::parse_str(&source.memory.id)?;
                ensure!(
                    source_ids.insert(&source.memory.id)
                        && source.vector.len() == operation.embedding_dimensions
                        && source.vector.iter().all(|v| v.is_finite()),
                    "invalid journal source vector/identity"
                );
                if let Some(realm) = &source.memory.realm_id {
                    uuid::Uuid::parse_str(realm)?;
                }
            }
            if let Some(winner) = &operation.dedup_winner {
                uuid::Uuid::parse_str(&winner.id)?;
            }
        }
    }
    let paired = if dir.join("manifest.json").exists() {
        let manifest: Manifest = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
        ensure!(
            manifest.version == 1
                && manifest.capabilities
                    == [
                        "logical-qdrant-v1",
                        "consolidation-dedup-v1",
                        "timeline-payload-v1"
                    ],
            "unsupported backup capabilities/version"
        );
        ensure!(
            manifest.evidence_schema.is_none() || manifest.evidence_schema == Some(1),
            "unsupported evidence schema"
        );
        let paired = manifest.evidence_schema.is_some();
        let required: BTreeMap<String, String> = BASE
            .iter()
            .copied()
            .chain(paired.then_some("evidence.sqlite3"))
            .map(|name| Ok((name.to_owned(), digest(&dir.join(name))?)))
            .collect::<Result<_>>()?;
        ensure!(
            manifest.files == required,
            "backup component integrity mismatch"
        );
        paired
    } else {
        false
    };
    ensure!(
        paired || !dir.join("evidence.sqlite3").exists(),
        "evidence database requires manifest"
    );
    if !paired {
        ensure!(
            memories.iter().all(|m| m.memory.evidence_refs.is_empty()),
            "evidence references require paired SQLite archive"
        );
        return Ok(false);
    }
    let conn = rusqlite::Connection::open_with_flags(
        dir.join("evidence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    ensure!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))? == 1,
        "unsupported evidence schema"
    );
    ensure!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "evidence integrity check failed"
    );
    let objects: Vec<(String, String)> = conn
        .prepare(
            "SELECT type,name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        objects == vec![("table".into(), "sources".into())],
        "unsupported evidence schema objects"
    );
    let columns: Vec<(String, String)> = conn
        .prepare("PRAGMA table_info(sources)")?
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        columns
            == [
                "source_id",
                "memory_id",
                "idempotency_key",
                "request",
                "record"
            ]
            .map(|n| (n.to_owned(), "TEXT".to_owned())),
        "unsupported evidence columns"
    );
    let mut stmt =
        conn.prepare("SELECT source_id,memory_id,idempotency_key,request,record FROM sources")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut records = HashMap::new();
    let mut memory_ids = std::collections::HashSet::new();
    let mut keys = std::collections::HashSet::new();
    for row in rows {
        let (source, memory, key, request, record) = row?;
        let r: SourceRecord = serde_json::from_str(&record)?;
        let (content, capture, scope, visibility): (String, CaptureRequest, Vec<String>, String) =
            serde_json::from_str(&request)?;
        uuid::Uuid::parse_str(&source)?;
        uuid::Uuid::parse_str(&memory)?;
        ensure!(
            r.schema_version == 1
                && r.source_id == source
                && r.memory_id == memory
                && !key.is_empty()
                && key.len() <= 128
                && !key.chars().any(char::is_control),
            "invalid evidence identity"
        );
        ensure!(
            r.content_hash == format!("{:x}", Sha256::digest(r.content.as_bytes()))
                && r.content == content.replace("\r\n", "\n").replace('\r', "\n")
                && r.scope == scope
                && r.visibility == visibility
                && capture.pre_redacted
                && r.redaction == "caller_pre_redacted"
                && r.speaker == capture.speaker
                && r.origin == capture.origin
                && r.agent == capture.agent
                && r.device == capture.device
                && r.session == capture.session,
            "invalid evidence content/request integrity"
        );
        ensure!(
            records.insert(source, r).is_none() && memory_ids.insert(memory) && keys.insert(key),
            "duplicate evidence identity"
        );
    }
    let mut ids = std::collections::HashSet::new();
    for snapshot in memories {
        let m = &snapshot.memory;
        ensure!(ids.insert(&m.id), "duplicate memory identity");
        for link in &m.evidence_refs {
            let r = records
                .get(&link.source_id)
                .ok_or_else(|| anyhow::anyhow!("missing evidence source"))?;
            ensure!(
                r.memory_id == m.id
                    && r.content == m.content
                    && r.content_hash == m.source_hash
                    && r.scope == m.tags
                    && r.visibility == m.visibility
                    && link.schema_version == 1
                    && link.start_byte == 0
                    && link.end_byte == r.content.len(),
                "invalid evidence linkage/scope/span"
            );
        }
        if let Some(r) = records.values().find(|r| r.memory_id == m.id) {
            ensure!(
                m.evidence_refs.len() == 1 && m.evidence_refs[0].source_id == r.source_id,
                "source projection missing evidence link"
            );
        }
    }
    Ok(true)
}
