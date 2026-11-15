//! Deterministic coverage for the MCP/CLI publication boundary.
//!
//! These tests run without Qdrant. They pin the guarantee the transports rely
//! on: for read-only tools, the fetch *and* the response serialization happen
//! inside one maintenance read scope, so a pending evidence erasure cannot
//! commit in between. They deliberately do not assert anything about delivery
//! (stdout, SSE, sockets) — bytes already written cannot be recalled.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use memex8::api::server::is_gated_retrieval_post;
use memex8::mcp::publish::{is_read_only_tool, tool_response_line};
use memex8::mcp::tools;
use memex8::storage::gate::MaintenanceGate;
use memex8::storage::qdrant::QdrantStore;
use serde::ser::Serializer;
use serde_json::{json, Value};
use tokio::sync::Notify;

const SETTLE: Duration = Duration::from_millis(250);

/// Serializer that records whether the gate's read side was still held when the
/// response body was turned into bytes.
struct ProbeSerializer<'a> {
    gate: &'a MaintenanceGate,
    held_during_serialize: Arc<AtomicBool>,
}

impl serde::Serialize for ProbeSerializer<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Taking the write side from this task would succeed if the read scope
        // had already been released.
        self.held_during_serialize
            .store(self.gate.try_write().is_err(), Ordering::SeqCst);
        serializer.serialize_str("pending erase fixture content")
    }
}

#[tokio::test]
async fn read_only_responses_are_serialized_inside_the_read_gate() {
    let gate = MaintenanceGate::new();
    let held = Arc::new(AtomicBool::new(false));
    let id = json!("req-1");
    let line = tool_response_line(&gate, "memex8_search", Some(&id), async {
        Ok(ProbeSerializer {
            gate: &gate,
            held_during_serialize: held.clone(),
        })
    })
    .await
    .unwrap();

    assert!(
        held.load(Ordering::SeqCst),
        "serialization escaped the read gate"
    );
    let envelope: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(envelope["jsonrpc"], "2.0");
    assert_eq!(envelope["id"], json!("req-1"));
    assert_eq!(envelope["result"], json!("pending erase fixture content"));
}

#[tokio::test]
async fn erasure_cannot_commit_between_fetch_and_response_bytes() {
    let gate = Arc::new(MaintenanceGate::new());
    let fetched = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let publish = tokio::spawn({
        let gate = gate.clone();
        let fetched = fetched.clone();
        let release = release.clone();
        async move {
            tool_response_line(&gate, "memex8_get", None, async move {
                fetched.notify_one();
                release.notified().await;
                Ok(json!({ "memory": { "content": "erased after publication" } }))
            })
            .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), fetched.notified())
        .await
        .expect("tool fetch ran");
    let erase = tokio::spawn({
        let gate = gate.clone();
        async move {
            let _write = gate.write().await;
        }
    });
    tokio::time::sleep(SETTLE).await;
    assert!(
        !erase.is_finished(),
        "erasure committed before the response bytes were constructed"
    );

    release.notify_one();
    let line = tokio::time::timeout(Duration::from_secs(5), publish)
        .await
        .expect("publication finished")
        .unwrap()
        .unwrap();
    assert!(line.contains("erased after publication"));
    tokio::time::timeout(Duration::from_secs(5), erase)
        .await
        .expect("erasure ran after publication")
        .unwrap();
}

#[tokio::test]
async fn writer_tools_are_not_gated_so_they_cannot_deadlock() {
    let gate = MaintenanceGate::new();
    assert!(!is_read_only_tool("memex8_store"));
    assert!(!is_read_only_tool("memex8_certify"));
    let line = tool_response_line(&gate, "memex8_store", None, async {
        // A writer takes the exclusive side; it must not queue behind a read
        // scope the publisher itself is holding.
        let _write = gate.write().await;
        Ok(json!({ "status": "stored" }))
    })
    .await
    .unwrap();
    assert!(line.contains("stored"));
}

#[tokio::test]
async fn nested_read_inside_gated_scope_does_not_deadlock_behind_erasure() {
    let gate = Arc::new(MaintenanceGate::new());
    let inside = Arc::new(Notify::new());
    let writer_queued = Arc::new(Notify::new());

    let reader = tokio::spawn({
        let gated = gate.clone();
        let nested_gate = gate.clone();
        let inside = inside.clone();
        let writer_queued = writer_queued.clone();
        async move {
            gated
                .read_gated(async move {
                    inside.notify_one();
                    writer_queued.notified().await;
                    // This is what engine read functions do inside a boundary gate.
                    let _nested = tokio::time::timeout(Duration::from_secs(2), nested_gate.read())
                        .await
                        .expect("nested read queued behind the erasure");
                    42
                })
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), inside.notified())
        .await
        .expect("read scope entered");
    let erase = tokio::spawn({
        let gate = gate.clone();
        async move {
            let _write = gate.write().await;
        }
    });
    tokio::time::sleep(SETTLE).await;
    assert!(
        !erase.is_finished(),
        "erasure ran while the read scope was open"
    );

    writer_queued.notify_one();
    let value = tokio::time::timeout(Duration::from_secs(5), reader)
        .await
        .expect("gated read finished")
        .unwrap();
    assert_eq!(value, 42);
    tokio::time::timeout(Duration::from_secs(5), erase)
        .await
        .expect("erasure ran once the read scope closed")
        .unwrap();
}

#[tokio::test]
async fn store_read_gated_covers_cli_result_construction() {
    // Client construction does not connect; no Qdrant is contacted here.
    let store = QdrantStore::new("http://127.0.0.1:9").await.unwrap();
    let gate_holder = store.clone();
    let inside = Arc::new(Notify::new());
    let writer_queued = Arc::new(Notify::new());

    let render = tokio::spawn({
        let store = store.clone();
        let inside = inside.clone();
        let writer_queued = writer_queued.clone();
        async move {
            store
                .read_gated(async move {
                    inside.notify_one();
                    writer_queued.notified().await;
                    // Engine reads acquire this nested permit; it must succeed.
                    let _permit = tokio::time::timeout(
                        Duration::from_secs(2),
                        gate_holder.maintenance_read_guard(),
                    )
                    .await
                    .expect("nested engine read deadlocked");
                    "rendered memory content".to_string()
                })
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), inside.notified())
        .await
        .expect("CLI render scope entered");
    let erase = tokio::spawn({
        let store = store.clone();
        async move {
            let _write = store.maintenance_guard().await;
        }
    });
    tokio::time::sleep(SETTLE).await;
    assert!(
        !erase.is_finished(),
        "erasure ran before CLI output was built"
    );

    writer_queued.notify_one();
    let output = tokio::time::timeout(Duration::from_secs(5), render)
        .await
        .expect("CLI render finished")
        .unwrap();
    assert_eq!(output, "rendered memory content");
    tokio::time::timeout(Duration::from_secs(5), erase)
        .await
        .expect("erasure ran after CLI output was built")
        .unwrap();
}

#[tokio::test]
async fn every_advertised_tool_is_classified_for_the_gate() {
    const WRITERS: [&str; 8] = [
        "memex8_store",
        "memex8_remember",
        "memex8_certify",
        "memex8_restore_duplicate",
        "memex8_unconsolidate",
        "memex8_ingest",
        "memex8_upvote",
        "memex8_graph_build",
    ];
    let advertised = tools::list_tools();
    assert!(!advertised.is_empty());
    for tool in &advertised {
        let name = tool["name"].as_str().unwrap();
        assert!(
            is_read_only_tool(name) || WRITERS.contains(&name),
            "{name} is neither gated as a read nor listed as a writer"
        );
        assert!(
            !(is_read_only_tool(name) && WRITERS.contains(&name)),
            "{name} is classified as both read and write"
        );
    }
    // The CLI reads memory content through the same gate.
    for name in ["memex8_search", "memex8_recall", "memex8_get"] {
        assert!(is_read_only_tool(name), "{name} must be read-gated");
    }
}

/// Regression: `upvote-by-content` publishes fetched content and performs its
/// upvote without taking the exclusive maintenance guard, so its POST must be
/// read-gated through response construction. Dropping the path reopens the
/// search -> erase -> `content_head` window.
#[test]
fn content_publishing_retrieval_posts_are_gated() {
    for path in [
        "/memories/search",
        "/memories/batch-get",
        "/memories/upvote-by-content",
    ] {
        assert!(
            is_gated_retrieval_post(path),
            "{path} must hold the read gate through response construction"
        );
    }
    // Writers and metadata-only POSTs must not be serialized under a read scope:
    // the subset that takes the exclusive side would deadlock behind it.
    for path in [
        "/memories",
        "/memories/ingest",
        "/memories/9f1c2d3e-0000-4000-8000-000000000000/upvote",
        "/memories/9f1c2d3e-0000-4000-8000-000000000000/downvote",
        "/memories/9f1c2d3e-0000-4000-8000-000000000000/certify",
        "/dreams",
        "/graph/build",
    ] {
        assert!(
            !is_gated_retrieval_post(path),
            "{path} is a writer or metadata route and must not be read-gated"
        );
    }
}
