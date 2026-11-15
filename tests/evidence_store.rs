use memex8::storage::evidence::{CaptureRequest, EvidenceStore};

fn assert_erased(path: &std::path::Path, source_id: &str, memory_id: &str, key: &str) {
    let store = EvidenceStore::open(path).unwrap();
    assert!(store.get(source_id).unwrap().is_none());
    assert!(!store.contains_memory(memory_id).unwrap());
    assert!(!store.has_records().unwrap());
    for text in ["original private content", "changed content"] {
        assert!(store
            .prepare(key, text, &capture(), &[], "private")
            .is_err());
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    for table in ["sources", "erasure_intents"] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for index in 0..row.as_ref().column_count() {
                if let rusqlite::types::ValueRef::Text(value) = row.get_ref(index).unwrap() {
                    let value = std::str::from_utf8(value).unwrap();
                    assert!(!value.contains("original private content"));
                    assert!(!value.contains("session-1"));
                    assert_ne!(value, key);
                }
            }
        }
    }
}

#[test]
fn erase_prepared_source_and_reconcile_intent_without_qdrant() {
    for reopen_before_finish in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let store = EvidenceStore::open(&path).unwrap();
        let key = "erase-private-key";
        let record = store
            .prepare(key, "original private content", &capture(), &[], "private")
            .unwrap();
        let intent = store.begin_erase(&record.memory_id).unwrap().unwrap();
        assert_eq!(intent.source_id, record.source_id);
        assert_eq!(intent.memory_id, record.memory_id);
        assert!(!intent.complete);
        assert_erased(&path, &record.source_id, &record.memory_id, key);
        assert_eq!(
            store
                .begin_erase(&record.memory_id)
                .unwrap()
                .unwrap()
                .source_id,
            record.source_id
        );
        if reopen_before_finish {
            drop(store);
            let store = EvidenceStore::open(&path).unwrap();
            assert_eq!(store.pending_erasures().unwrap().len(), 1);
            store.finish_erase(&record.memory_id).unwrap();
            store.finish_erase(&record.memory_id).unwrap();
            assert!(store.pending_erasures().unwrap().is_empty());
        } else {
            store.finish_erase(&record.memory_id).unwrap();
        }
        assert!(
            EvidenceStore::open(&path)
                .unwrap()
                .begin_erase(&record.memory_id)
                .unwrap()
                .unwrap()
                .complete
        );
        assert_erased(&path, &record.source_id, &record.memory_id, key);
    }
}

#[test]
fn missing_v2_erasure_table_fails_closed_without_recreation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.sqlite3");
    let store = EvidenceStore::open(&path).unwrap();
    let row = store
        .prepare("short", "secret", &capture(), &[], "private")
        .unwrap();
    store.begin_erase(&row.memory_id).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("DROP TABLE erasure_intents", []).unwrap();
    drop(conn);
    assert!(EvidenceStore::open(&path).is_err());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='erasure_intents')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!exists);
}

#[test]
fn migration_preserves_v1_sources_and_rejects_future_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.sqlite3");
    let record = EvidenceStore::open(&path)
        .unwrap()
        .prepare(
            "legacy-key",
            "original private content",
            &capture(),
            &[],
            "private",
        )
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("DROP TABLE erasure_intents", []).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    let store = EvidenceStore::open(&path).unwrap();
    assert_eq!(
        store.get(&record.source_id).unwrap().unwrap().content,
        record.content
    );
    assert_eq!(store.pending_erasures().unwrap().len(), 0);
    store.begin_erase(&record.memory_id).unwrap().unwrap();
    assert_erased(&path, &record.source_id, &record.memory_id, "legacy-key");
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
}

#[test]
fn short_erased_key_uses_private_mac_and_missing_key_fails_closed() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.sqlite3");
    let store = EvidenceStore::open(&path).unwrap();
    let record = store
        .prepare("x", "secret", &capture(), &[], "private")
        .unwrap();
    store.begin_erase(&record.memory_id).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let digest: String = conn
        .query_row("SELECT key_digest FROM erasure_intents", [], |r| r.get(0))
        .unwrap();
    assert_ne!(digest, format!("{:x}", Sha256::digest(b"x")));
    let key_path = memex8::storage::evidence::replay_key_path(&path);
    assert_eq!(
        std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::read(&key_path).unwrap().len(), 32);
    drop(store);
    assert!(EvidenceStore::open(&path)
        .unwrap()
        .prepare("x", "changed content", &capture(), &[], "private")
        .is_err());
    std::fs::remove_file(key_path).unwrap();
    assert!(EvidenceStore::open(&path).is_err());
}

#[test]
fn unkeyed_v2_intents_refuse_migration_without_changing_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.sqlite3");
    let store = EvidenceStore::open(&path).unwrap();
    let row = store
        .prepare("x", "secret", &capture(), &[], "private")
        .unwrap();
    store.begin_erase(&row.memory_id).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    std::fs::remove_file(memex8::storage::evidence::replay_key_path(&path)).unwrap();
    assert!(EvidenceStore::open(&path).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(!memex8::storage::evidence::replay_key_path(&path).exists());
}

fn capture() -> CaptureRequest {
    serde_json::from_value(serde_json::json!({
        "speaker": "user", "origin": "explicit_rest", "pre_redacted": true,
        "agent": "fixture", "session": "session-1"
    }))
    .unwrap()
}

#[test]
fn ledger_refuses_future_schema_without_downgrading_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    assert!(EvidenceStore::open(&path).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        99
    );
}

#[test]
fn new_ledger_is_private_and_reads_exact_source_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.sqlite3");
    let store = EvidenceStore::open(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let record = store
        .prepare("k", "évidence", &capture(), &[], "private")
        .unwrap();
    assert_eq!(
        store.get(&record.source_id).unwrap().unwrap().content,
        "évidence"
    );
    assert!(store.contains_memory(&record.memory_id).unwrap());
    assert!(store.get("file:///etc/passwd").unwrap().is_none());
}

#[test]
fn invalid_capture_leaves_no_sensitive_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = EvidenceStore::open(&dir.path().join("evidence.sqlite3")).unwrap();
    for (key, text, speaker, redacted, visibility) in [
        ("", "secret", "user", true, "private"),
        ("k", "", "user", true, "private"),
        ("k", "secret", "system", true, "private"),
        ("k", "secret", "user", false, "private"),
        ("k", "secret", "user", true, "unknown"),
    ] {
        let mut c = capture();
        c.speaker = speaker.into();
        c.pre_redacted = redacted;
        assert!(store.prepare(key, text, &c, &[], visibility).is_err());
    }
    assert!(store
        .prepare("k", &"x".repeat(65537), &capture(), &[], "private")
        .is_err());
    assert!(!store.has_records().unwrap());
}

#[test]
fn durable_capture_retries_keep_identity_and_reject_changed_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.sqlite3");
    let first = {
        let store = EvidenceStore::open(&path).unwrap();
        store
            .prepare(
                "retry-1",
                "hello\r\nworld",
                &capture(),
                &["project:one".into()],
                "private",
            )
            .unwrap()
    };
    uuid::Uuid::parse_str(&first.source_id).unwrap();
    uuid::Uuid::parse_str(&first.memory_id).unwrap();
    let store = EvidenceStore::open(&path).unwrap();
    let retry = store
        .prepare(
            "retry-1",
            "hello\r\nworld",
            &capture(),
            &["project:one".into()],
            "private",
        )
        .unwrap();
    assert_eq!(retry.source_id, first.source_id);
    assert_eq!(retry.memory_id, first.memory_id);
    assert_eq!(retry.content, "hello\nworld");
    assert_eq!(retry.speaker, "user");
    assert_eq!(retry.scope, vec!["project:one"]);
    assert!(store
        .prepare(
            "retry-1",
            "different",
            &capture(),
            &["project:one".into()],
            "private"
        )
        .is_err());
    assert!(store.has_records().unwrap());
}
