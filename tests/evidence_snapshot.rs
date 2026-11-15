use memex8::storage::evidence::{CaptureRequest, EvidenceStore};
#[test]
fn snapshot_preserves_committed_wal_and_pending_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.sqlite3");
    let store = EvidenceStore::open(&path).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let capture: CaptureRequest = serde_json::from_value(
        serde_json::json!({"speaker":"user","origin":"explicit_rest","pre_redacted":true}),
    )
    .unwrap();
    let first = store
        .prepare("pending", "exact\r\né", &capture, &[], "private")
        .unwrap();
    let snapshot = dir.path().join("snapshot.sqlite3");
    store.snapshot(&snapshot).unwrap();
    let restored = EvidenceStore::open(&snapshot).unwrap();
    let retry = restored
        .prepare("pending", "exact\r\né", &capture, &[], "private")
        .unwrap();
    assert_eq!(retry.source_id, first.source_id);
    assert_eq!(retry.memory_id, first.memory_id);
    assert_eq!(retry.content, "exact\né");
}
