use memex8::storage::evidence::{CaptureRequest, EvidenceStore};

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
